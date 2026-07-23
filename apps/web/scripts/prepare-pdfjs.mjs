import {
  access,
  cp,
  mkdir,
  readFile,
  rm,
  writeFile,
} from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const source = resolve(root, "node_modules", "pdfjs-dist");
const destination = resolve(root, "assets", "vendor", "pdfjs");
const packageMetadata = JSON.parse(
  await readFile(resolve(source, "package.json"), "utf8"),
);
const versionMarker = resolve(destination, ".lumi-pdfjs-version");

let alreadyPrepared = false;
try {
  const [preparedVersion] = await Promise.all([
    readFile(versionMarker, "utf8"),
    access(resolve(destination, "pdf.mjs")),
    access(resolve(destination, "pdf.worker.mjs")),
    access(resolve(destination, "cmaps")),
    access(resolve(destination, "standard_fonts")),
  ]);
  alreadyPrepared = preparedVersion.trim() === packageMetadata.version;
} catch {
  alreadyPrepared = false;
}

if (!alreadyPrepared) {
  await rm(destination, { recursive: true, force: true });
  await mkdir(destination, { recursive: true });
  await Promise.all([
    cp(resolve(source, "build", "pdf.mjs"), resolve(destination, "pdf.mjs")),
    cp(
      resolve(source, "build", "pdf.worker.mjs"),
      resolve(destination, "pdf.worker.mjs"),
    ),
    cp(
      resolve(source, "standard_fonts"),
      resolve(destination, "standard_fonts"),
      { recursive: true },
    ),
    cp(resolve(source, "cmaps"), resolve(destination, "cmaps"), {
      recursive: true,
    }),
  ]);
  await writeFile(versionMarker, `${packageMetadata.version}\n`);
}
