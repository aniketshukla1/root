/**
 * Film grain, made once: a small square of noise painted to a canvas and kept
 * as a data URL, then tiled and jittered by CSS. Nothing is redrawn per frame.
 */
let cached: string | undefined;

export function grainUrl(): string {
  if (cached !== undefined) return cached;
  try {
    const size = 160;
    const canvas = document.createElement("canvas");
    canvas.width = size;
    canvas.height = size;
    const ctx = canvas.getContext("2d");
    if (!ctx) return (cached = "");
    const image = ctx.createImageData(size, size);
    for (let i = 0; i < image.data.length; i += 4) {
      const v = Math.random() * 255;
      image.data[i] = v;
      image.data[i + 1] = v;
      image.data[i + 2] = v;
      image.data[i + 3] = Math.random() * 46;
    }
    ctx.putImageData(image, 0, 0);
    cached = canvas.toDataURL("image/png");
  } catch {
    cached = "";
  }
  return cached;
}
