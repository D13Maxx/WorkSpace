// Copy pdf.js's data folders into public/pdfjs, so the PDF viewer can load
// them from the app itself. Without them, fonts a PDF does not embed render
// as nothing, and scanned pages (JBIG2, JPEG 2000 images) come out blank.
// Runs before `dev` and `build`, so the copy always matches pdfjs-dist.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const app = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const from = path.join(app, "node_modules/pdfjs-dist");
const to = path.join(app, "public/pdfjs");
fs.rmSync(to, { recursive: true, force: true });
for (const dir of ["cmaps", "standard_fonts", "wasm", "iccs"]) {
  fs.cpSync(path.join(from, dir), path.join(to, dir), { recursive: true });
}
