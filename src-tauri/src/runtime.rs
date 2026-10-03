//! Where a connection actually runs, as declared — and whether root agrees.
//!
//! `identify` reads an image name and a published port and concludes that a
//! container is a Postgres. That is reading rather than guessing, but it is
//! still root talking to itself: nothing checks it, so a wrong answer is
//! wrong quietly, and an operator has no way to tell a correct inference from
//! a plausible one.
//!
//! A declaration fixes that. When somebody says "this connection is the
//! container `orders-db` on the `prod-docker` host", root has something to be
//! wrong against. The two are then reconciled on every poll and the
//! disagreements are the output — which is the whole point, because a
//! disagreement is either root being wrong or the estate having changed, and
//! both are worth knowing.
//!
//! ## Declared, and optional
//!
//! Nothing here is required. A connection with no runtime declared behaves
//! exactly as before and root's inference stands unchecked, which is the
//! honest default — most people will not fill this in, and a field that makes
//! the product worse when left empty is a field that should not exist.
//!
//! What it buys when it is filled in:
//!
//! - **the inference gets marked.** Root's port match either agrees with the
//!   declaration or it does not, and saying which is more useful than either
//!   one alone.
//! - **the estate gets a coverage answer.** Every container that looks like a
//!   service and has no connection pointed at it is a gap, and the only way to
//!   know a gap is deliberate is for somebody to have said so.
//!
//! ## Why not just trust the port match
//!
//! Because it is silent in exactly the cases that matter. A service on a
//! non-standard port, a container reached through a proxy, a pod with no
//! published port at all: root infers nothing and reports nothing, which reads
//! identically to "everything is fine". A declaration turns that silence into
//! a checkable claim.

use serde::{Deserialize, Serialize};

use crate::model::ServiceKind;

/// What kind of thing a connection is pointed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeKind {
    /// Not declared. Root's inference stands unchecked, which is the default
    /// and is fine — it is what happens today.
    #[default]
    Undeclared,
    /// A container on a Docker or Podman host.
    Container,
    /// A pod in a Kubernetes cluster.
    Pod,
    /// A process on a machine — a VM, bare metal, someone's laptop.
    Host,
    /// Something a cloud provider runs: RDS, ElastiCache, MemoryDB. There is
    /// no container to find and root should stop looking for one.
    Managed,
}

impl RuntimeKind {
    /// Whether root should expect to find this among another connection's
    /// components.
    pub fn is_hosted(self) -> bool {
        matches!(self, RuntimeKind::Container | RuntimeKind::Pod)
    }

    pub fn label(self) -> &'static str {
        match self {
            RuntimeKind::Undeclared => "not declared",
            RuntimeKind::Container => "container",
            RuntimeKind::Pod => "pod",
            RuntimeKind::Host => "host process",
            RuntimeKind::Managed => "managed service",
        }
    }
}

/// Where a connection runs, as somebody has said it does.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Runtime {
    pub kind: RuntimeKind,
    /// The connection that hosts it — a Docker host or a Kubernetes cluster.
    ///
    /// Optional even for a container: an operator may know a thing is
    /// containerised without root watching the host it runs on, and that is
    /// still worth recording.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_profile_id: Option<String>,
    /// The container or pod name, as the host reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Runtime {
    pub fn is_declared(&self) -> bool {
        self.kind != RuntimeKind::Undeclared
    }

    /// Whether this names a specific component root can go and look for.
    pub fn locatable(&self) -> Option<(&str, &str)> {
        if !self.kind.is_hosted() {
            return None;
        }
        Some((self.host_profile_id.as_deref()?, self.name.as_deref()?))
    }
}

/// Where root's inference and the declaration part company.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
// `rename_all` renames the variants only. Without `rename_all_fields` a
// field like `window_secs` reached the UI in snake_case while every reader
// there asks for `windowSecs` — the fixtures were written in camelCase, so
// only a real backend showed it, as "undefined" in the sentence.
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Discrepancy {
    /// Declared to run on a connection that is not configured. Usually a
    /// deleted host or a typo, and either way the declaration is now fiction.
    HostNotConfigured { host_profile_id: String },
    /// The host is configured but has no component by that name. The thing
    /// moved, was renamed, or never existed under that name.
    NotFound { host: String, name: String },
    /// Root's port match landed on a different component than the one
    /// declared. Exactly one of the two is right and root cannot tell which,
    /// so it reports both rather than picking.
    Disagrees {
        host: String,
        declared: String,
        inferred: String,
    },
    /// The named container exists but is not serving the port its image
    /// implies. Often correct — a sidecar, a job — and worth confirming.
    NotServing { host: String, name: String },
    /// Declared as a container or pod with no host or name, so nothing can be
    /// checked. Half a declaration is a claim nobody can verify.
    Incomplete,
}

impl Discrepancy {
    /// One line an operator can act on.
    pub fn describe(&self) -> String {
        match self {
            Discrepancy::HostNotConfigured { host_profile_id } => format!(
                "declared to run on `{host_profile_id}`, which is not a configured connection — \
                 the host was probably removed, and this declaration now describes nothing"
            ),
            Discrepancy::NotFound { host, name } => format!(
                "declared as `{name}` on {host}, which has no component by that name — it was \
                 renamed, moved, or is not running"
            ),
            Discrepancy::Disagrees {
                host,
                declared,
                inferred,
            } => format!(
                "declared as `{declared}` on {host}, but the published port matches `{inferred}` \
                 instead. One of the two is wrong and root cannot tell which"
            ),
            Discrepancy::NotServing { host, name } => format!(
                "`{name}` on {host} exists but is not publishing the port its image implies, so \
                 it may be a sidecar or a job rather than the service itself"
            ),
            Discrepancy::Incomplete => {
                "declared as hosted, but without a host and a name there is nothing to check it \
                 against"
                    .to_string()
            }
        }
    }
}

/// One component root can see, as far as reconciliation cares.
pub struct Component<'a> {
    pub name: &'a str,
    /// What its image says it is, if anything.
    pub identified: Option<ServiceKind>,
    /// Whether it publishes the port that kind serves on.
    pub serving: bool,
    /// The connection root's own port match linked it to.
    pub watched_by: Option<&'a str>,
}

/// Check a declaration against what root can see.
///
/// `components` are the host connection's components. An empty result means
/// the declaration and the inference agree, or that there was nothing to
/// check — those are different, and `Runtime::is_declared` tells them apart.
pub fn reconcile(
    profile_id: &str,
    runtime: &Runtime,
    host_exists: bool,
    host_label: &str,
    components: &[Component<'_>],
) -> Vec<Discrepancy> {
    if !runtime.kind.is_hosted() {
        return Vec::new();
    }

    let Some(host_id) = runtime.host_profile_id.as_deref() else {
        return vec![Discrepancy::Incomplete];
    };
    if runtime.name.as_deref().is_none_or(str::is_empty) {
        return vec![Discrepancy::Incomplete];
    }
    if !host_exists {
        return vec![Discrepancy::HostNotConfigured {
            host_profile_id: host_id.to_string(),
        }];
    }

    let declared_name = runtime.name.as_deref().unwrap_or_default();
    let Some(declared) = components.iter().find(|c| c.name == declared_name) else {
        // Before calling it missing, see whether root's own match found
        // something — a rename produces both facts at once, and reporting the
        // disagreement is far more useful than reporting an absence.
        if let Some(inferred) = components
            .iter()
            .find(|c| c.watched_by == Some(profile_id))
        {
            return vec![Discrepancy::Disagrees {
                host: host_label.to_string(),
                declared: declared_name.to_string(),
                inferred: inferred.name.to_string(),
            }];
        }
        return vec![Discrepancy::NotFound {
            host: host_label.to_string(),
            name: declared_name.to_string(),
        }];
    };

    let mut out = Vec::new();

    // Root matched this connection to a different component than the one
    // declared. Both claims are specific and they cannot both be right.
    if let Some(inferred) = components
        .iter()
        .find(|c| c.watched_by == Some(profile_id))
    {
        if inferred.name != declared_name {
            out.push(Discrepancy::Disagrees {
                host: host_label.to_string(),
                declared: declared_name.to_string(),
                inferred: inferred.name.to_string(),
            });
        }
    }

    // Named correctly but not actually serving. Frequently fine, which is why
    // it is reported rather than treated as an error.
    if declared.identified.is_some() && !declared.serving {
        out.push(Discrepancy::NotServing {
            host: host_label.to_string(),
            name: declared_name.to_string(),
        });
    }

    out
}

/// A component that looks like a service and has nothing watching it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Uncovered {
    pub host_profile_id: String,
    pub host_label: String,
    pub name: String,
    pub kind: ServiceKind,
}

/// Components that look like services nobody is monitoring properly.
///
/// The coverage question, and the one a declaration makes answerable: a
/// container identified as a Redis, publishing 6379, with no connection
/// pointed at it, is either an oversight or a deliberate choice — and root
/// cannot tell which, so it lists them and lets somebody say.
///
/// Only serving components count. A container carrying a database image
/// without running one needs no connection, and listing it would bury the
/// real gaps in noise.
pub fn uncovered(
    host_profile_id: &str,
    host_label: &str,
    components: &[Component<'_>],
    declared_names: &[String],
) -> Vec<Uncovered> {
    components
        .iter()
        .filter(|c| c.serving)
        .filter(|c| c.watched_by.is_none())
        .filter(|c| !declared_names.iter().any(|n| n == c.name))
        .filter_map(|c| {
            Some(Uncovered {
                host_profile_id: host_profile_id.to_string(),
                host_label: host_label.to_string(),
                name: c.name.to_string(),
                kind: c.identified?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container<'a>(
        name: &'a str,
        kind: Option<ServiceKind>,
        serving: bool,
        watched_by: Option<&'a str>,
    ) -> Component<'a> {
        Component {
            name,
            identified: kind,
            serving,
            watched_by,
        }
    }

    fn declared(name: &str) -> Runtime {
        Runtime {
            kind: RuntimeKind::Container,
            host_profile_id: Some("docker".into()),
            name: Some(name.into()),
        }
    }

    // -------------------------------------------------------- agreement

    /// The ordinary case: somebody said where it runs, root's port match found
    /// the same thing, nothing to report.
    #[test]
    fn a_declaration_root_agrees_with_reports_nothing() {
        let components = vec![container(
            "orders-db",
            Some(ServiceKind::Postgres),
            true,
            Some("db"),
        )];
        assert!(reconcile("db", &declared("orders-db"), true, "prod-docker", &components).is_empty());
    }

    /// The default. Root's inference stands unchecked, which is what happens
    /// today and must keep happening for anyone who never fills this in.
    #[test]
    fn an_undeclared_connection_is_not_nagged_about() {
        let components = vec![container("anything", None, false, None)];
        assert!(reconcile("db", &Runtime::default(), true, "prod-docker", &components).is_empty());
    }

    /// A managed service has no container to find, and root should stop
    /// looking rather than reporting the absence as a problem.
    #[test]
    fn a_managed_service_is_never_looked_for_among_containers() {
        let runtime = Runtime {
            kind: RuntimeKind::Managed,
            host_profile_id: None,
            name: None,
        };
        assert!(reconcile("db", &runtime, false, "", &[]).is_empty());
    }

    // ------------------------------------------------------ disagreement

    /// The reason the feature exists: root inferred one thing and a human said
    /// another. Exactly one is right and root cannot tell which, so it says so
    /// rather than silently preferring itself.
    #[test]
    fn root_matching_a_different_container_than_declared_is_reported_as_a_disagreement() {
        let components = vec![
            container("orders-db", Some(ServiceKind::Postgres), true, None),
            container("orders-db-replica", Some(ServiceKind::Postgres), true, Some("db")),
        ];

        let found = reconcile("db", &declared("orders-db"), true, "prod-docker", &components);
        assert_eq!(
            found,
            vec![Discrepancy::Disagrees {
                host: "prod-docker".into(),
                declared: "orders-db".into(),
                inferred: "orders-db-replica".into(),
            }]
        );
        assert!(found[0].describe().contains("cannot tell which"));
    }

    /// A rename produces an absence and a match at the same moment. Reporting
    /// the disagreement names the new container; reporting the absence just
    /// says something is gone.
    #[test]
    fn a_rename_is_reported_as_a_disagreement_rather_than_an_absence() {
        let components = vec![container(
            "orders-db-v2",
            Some(ServiceKind::Postgres),
            true,
            Some("db"),
        )];

        let found = reconcile("db", &declared("orders-db"), true, "prod-docker", &components);
        assert!(
            matches!(found[0], Discrepancy::Disagrees { .. }),
            "{found:#?}"
        );
    }

    #[test]
    fn a_declared_container_that_is_not_there_is_reported_as_missing() {
        let components = vec![container("something-else", None, false, None)];
        let found = reconcile("db", &declared("orders-db"), true, "prod-docker", &components);
        assert_eq!(
            found,
            vec![Discrepancy::NotFound {
                host: "prod-docker".into(),
                name: "orders-db".into(),
            }]
        );
    }

    #[test]
    fn a_declaration_pointing_at_a_removed_host_says_so() {
        let found = reconcile("db", &declared("orders-db"), false, "", &[]);
        assert_eq!(
            found,
            vec![Discrepancy::HostNotConfigured {
                host_profile_id: "docker".into(),
            }]
        );
    }

    /// Half a declaration is a claim nobody can verify, so it is reported
    /// rather than quietly ignored.
    #[test]
    fn an_incomplete_declaration_is_reported_rather_than_ignored() {
        for runtime in [
            Runtime {
                kind: RuntimeKind::Container,
                host_profile_id: None,
                name: Some("orders-db".into()),
            },
            Runtime {
                kind: RuntimeKind::Pod,
                host_profile_id: Some("cluster".into()),
                name: None,
            },
        ] {
            assert_eq!(
                reconcile("db", &runtime, true, "host", &[]),
                vec![Discrepancy::Incomplete]
            );
        }
    }

    /// Named right, but the container is not publishing the port its image
    /// implies. Often a sidecar and perfectly correct — which is why it is a
    /// note rather than an error.
    #[test]
    fn a_declared_container_that_is_not_serving_is_flagged_for_confirmation() {
        let components = vec![container(
            "orders-db",
            Some(ServiceKind::Postgres),
            false,
            Some("db"),
        )];
        let found = reconcile("db", &declared("orders-db"), true, "prod-docker", &components);
        assert_eq!(
            found,
            vec![Discrepancy::NotServing {
                host: "prod-docker".into(),
                name: "orders-db".into(),
            }]
        );
    }

    // ---------------------------------------------------------- coverage

    /// The other half: a service running in a container that nothing watches.
    #[test]
    fn a_serving_container_with_no_connection_is_listed_as_uncovered() {
        let components = vec![
            container("orders-db", Some(ServiceKind::Postgres), true, Some("db")),
            container("sessions", Some(ServiceKind::Redis), true, None),
        ];

        let gaps = uncovered("docker", "prod-docker", &components, &[]);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].name, "sessions");
        assert_eq!(gaps[0].kind, ServiceKind::Redis);
    }

    /// A container carrying a database image without running one needs no
    /// connection, and listing it would bury the real gaps.
    #[test]
    fn a_container_that_is_not_serving_is_not_a_coverage_gap() {
        let components = vec![container(
            "db-migrations",
            Some(ServiceKind::Postgres),
            false,
            None,
        )];
        assert!(uncovered("docker", "prod-docker", &components, &[]).is_empty());
    }

    /// Declaring it is the way to say a gap is deliberate — or to cover a
    /// service root's port match cannot see, which is the case the whole
    /// declaration exists for.
    #[test]
    fn declaring_a_container_covers_it_even_when_the_port_match_cannot() {
        let components = vec![container("sessions", Some(ServiceKind::Redis), true, None)];

        assert_eq!(uncovered("docker", "prod-docker", &components, &[]).len(), 1);
        assert!(
            uncovered("docker", "prod-docker", &components, &["sessions".to_string()]).is_empty(),
            "a declared container is covered even with no port to match on"
        );
    }

    #[test]
    fn an_unrecognised_container_is_never_a_coverage_gap() {
        let components = vec![container("orders-api", None, true, None)];
        assert!(uncovered("docker", "prod-docker", &components, &[]).is_empty());
    }
}
