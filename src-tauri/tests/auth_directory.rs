//! Accounts and sessions, against a real Postgres.
//!
//! The properties worth holding open here are the ones that are invisible
//! until somebody attacks them:
//!
//!   - A wrong password and an unknown address fail the same way, in the same
//!     message. A distinct "no such account" tells whoever is guessing which
//!     addresses are worth spending time on.
//!   - What is stored is not what was handed out. A database dump should not
//!     contain a single replayable session token or a recoverable password.
//!   - Disabling somebody ends their sessions now, not when their token
//!     happens to expire.
//!
//! Skips cleanly when no server is reachable, the same as `store.rs`:
//!
//! ```bash
//! docker run --rm -d -p 55433:5432 -e POSTGRES_PASSWORD=testpw \
//!   -e POSTGRES_DB=roottest --name root-store-test postgres:16-alpine
//! ```

use inframonitoring_lib::auth::{Directory, Role};

const URL: &str = "postgres://postgres:testpw@localhost:55433/roottest";

/// A directory in a schema of this test's own, so the suite runs in parallel.
async fn directory(test: &str) -> Option<Directory> {
    let admin = match sqlx::PgPool::connect(URL).await {
        Ok(pool) => pool,
        Err(e) => {
            eprintln!("skipped: no Postgres at {URL} ({e})");
            return None;
        }
    };
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "DROP SCHEMA IF EXISTS {test} CASCADE"
    )))
    .execute(&admin)
    .await
    .expect("drop schema");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {test}")))
        .execute(&admin)
        .await
        .expect("create schema");

    let scoped = sqlx::PgPool::connect(&format!("{URL}?options=-csearch_path%3D{test}"))
        .await
        .expect("connect in schema");
    Some(Directory::open(scoped).await.expect("open"))
}

const PASSWORD: &str = "correct horse battery staple";

#[tokio::test]
async fn the_first_account_is_an_administrator_and_the_next_is_not() {
    // A server whose first account was a viewer would have nobody able to
    // promote anyone, which is a server somebody has to delete and redeploy.
    let Some(dir) = directory("first_account_is_admin").await else {
        return;
    };

    let first = dir.create("ops@example.com", PASSWORD, "Ops", None).await.unwrap();
    assert_eq!(first.role, Role::Admin);

    let second = dir.create("dev@example.com", PASSWORD, "Dev", None).await.unwrap();
    assert_eq!(second.role, Role::Viewer, "least privilege by default");
}

#[tokio::test]
async fn a_wrong_password_and_an_unknown_address_fail_identically() {
    let Some(dir) = directory("failures_are_identical").await else {
        return;
    };
    dir.create("ops@example.com", PASSWORD, "Ops", None).await.unwrap();

    let wrong_password = dir.sign_in("ops@example.com", "not the password").await;
    let no_such_account = dir.sign_in("nobody@example.com", PASSWORD).await;

    let a = wrong_password.unwrap_err().to_string();
    let b = no_such_account.unwrap_err().to_string();
    assert_eq!(
        a, b,
        "the two failures must be indistinguishable to whoever is guessing"
    );
}

#[tokio::test]
async fn a_session_token_is_never_stored() {
    // The point of hashing them: a database that leaks hands over no session
    // anybody can replay.
    let Some(dir) = directory("token_is_never_stored").await else {
        return;
    };
    dir.create("ops@example.com", PASSWORD, "Ops", None).await.unwrap();
    let (token, _) = dir.sign_in("ops@example.com", PASSWORD).await.unwrap();

    let pool = sqlx::PgPool::connect(&format!(
        "{URL}?options=-csearch_path%3Dtoken_is_never_stored"
    ))
    .await
    .unwrap();
    let rows: Vec<String> = sqlx::query_scalar("SELECT token_hash FROM sessions")
        .fetch_all(&pool)
        .await
        .unwrap();

    assert_eq!(rows.len(), 1);
    assert_ne!(rows[0], token, "the raw token is in the table");
    assert!(!rows[0].contains(&token));
}

#[tokio::test]
async fn a_password_is_not_recoverable_from_the_table() {
    let Some(dir) = directory("password_not_recoverable").await else {
        return;
    };
    dir.create("ops@example.com", PASSWORD, "Ops", None).await.unwrap();

    let pool = sqlx::PgPool::connect(&format!(
        "{URL}?options=-csearch_path%3Dpassword_not_recoverable"
    ))
    .await
    .unwrap();
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert!(hash.starts_with("$argon2id$"), "{hash}");
    assert!(!hash.contains("correct horse"));
}

#[tokio::test]
async fn a_session_identifies_its_owner_and_survives_being_used() {
    let Some(dir) = directory("session_identifies_owner").await else {
        return;
    };
    dir.create("ops@example.com", PASSWORD, "Ops Person", None).await.unwrap();
    let (token, signed_in) = dir.sign_in("OPS@Example.com", PASSWORD).await.unwrap();

    assert_eq!(signed_in.role, Role::Admin);
    assert_eq!(signed_in.email, "ops@example.com", "normalized on the way in");

    let identity = dir.identify(&token).await.expect("the session resolves");
    assert_eq!(identity.display_name, "Ops Person");
    assert_eq!(identity.user_id, signed_in.user_id);

    // Used twice: the sliding expiry must not invalidate it.
    assert!(dir.identify(&token).await.is_some());
}

#[tokio::test]
async fn a_signed_out_session_stops_working_immediately() {
    let Some(dir) = directory("signed_out_stops_working").await else {
        return;
    };
    dir.create("ops@example.com", PASSWORD, "Ops", None).await.unwrap();
    let (token, _) = dir.sign_in("ops@example.com", PASSWORD).await.unwrap();

    assert!(dir.identify(&token).await.is_some());
    dir.sign_out(&token).await.unwrap();
    assert!(dir.identify(&token).await.is_none());
}

#[tokio::test]
async fn disabling_somebody_ends_their_sessions_now() {
    // Not when the token expires. Somebody who has just left should not have
    // a working session for the rest of the week.
    let Some(dir) = directory("disabling_ends_sessions").await else {
        return;
    };
    let account = dir.create("ops@example.com", PASSWORD, "Ops", None).await.unwrap();
    let (first, _) = dir.sign_in("ops@example.com", PASSWORD).await.unwrap();
    let (second, _) = dir.sign_in("ops@example.com", PASSWORD).await.unwrap();

    dir.update(&account.id, None, Some(true)).await.unwrap();

    assert!(dir.identify(&first).await.is_none(), "one session survived");
    assert!(dir.identify(&second).await.is_none(), "another survived");
    assert!(
        dir.sign_in("ops@example.com", PASSWORD).await.is_err(),
        "a disabled account signed back in"
    );
}

#[tokio::test]
async fn a_role_change_takes_effect_on_the_next_request() {
    let Some(dir) = directory("role_change_takes_effect").await else {
        return;
    };
    dir.create("ops@example.com", PASSWORD, "Ops", None).await.unwrap();
    let viewer = dir
        .create("dev@example.com", PASSWORD, "Dev", None)
        .await
        .unwrap();
    let (token, _) = dir.sign_in("dev@example.com", PASSWORD).await.unwrap();
    assert_eq!(dir.identify(&token).await.unwrap().role, Role::Viewer);

    dir.update(&viewer.id, Some(Role::Operator), None).await.unwrap();

    // The session is unchanged; the role is read fresh every time, so a
    // promotion does not require signing out and back in.
    assert_eq!(dir.identify(&token).await.unwrap().role, Role::Operator);
}

#[tokio::test]
async fn the_same_address_cannot_be_registered_twice() {
    let Some(dir) = directory("no_duplicate_addresses").await else {
        return;
    };
    dir.create("ops@example.com", PASSWORD, "Ops", None).await.unwrap();
    let again = dir.create("OPS@EXAMPLE.COM", PASSWORD, "Impostor", None).await;
    assert!(again.is_err(), "case should not create a second account");
}

#[tokio::test]
async fn a_short_password_is_refused_before_it_is_hashed() {
    let Some(dir) = directory("short_password_refused").await else {
        return;
    };
    let error = dir
        .create("ops@example.com", "short", "Ops", None)
        .await
        .expect_err("too short");
    assert!(error.to_string().contains("12 characters"), "{error}");
    assert_eq!(dir.count().await.unwrap(), 0, "a rejected account was created");
}
