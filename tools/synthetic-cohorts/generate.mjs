import { createHash } from "node:crypto";
import { existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { resolve, join } from "node:path";

const seed = "baleyg-synthetic-cohorts-v1";
const languages = [
  ["java", "java"], ["rust", "rs"], ["python", "py"], ["javascript", "js"],
];
const sizes = [
  ["small", 25, 1677722],
  ["medium", 250, 16777216],
  ["large", 2500, 134217728],
];

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function source(size, language, index, target) {
  const className = `C${size}${String(index).padStart(4, "0")}`;
  const lines = language === "java" ? [`public class ${className} {`] : [];
  for (let declaration = 0; declaration < 224; declaration++) {
    const digest = sha256(Buffer.from(
      `${seed}\0${size}\0${language}\0${index}\0${declaration}`, "utf8",
    ));
    const name = `f${declaration.toString(36)}_${digest.slice(0, 6)}`;
    const value = parseInt(digest.slice(12, 19), 16);
    if (language === "java") lines.push(`static int ${name}(){return ${value};}`);
    if (language === "rust") lines.push(`fn ${name}() -> i32 { ${value} }`);
    if (language === "python") lines.push(`def ${name}(): return ${value}`);
    if (language === "javascript") lines.push(`function ${name}() { return ${value}; }`);
  }
  if (language === "java") lines.push("}");
  const body = `${lines.join("\n")}\n`;
  const prefix = language === "python" ? "# " : "// ";
  const remaining = target - Buffer.byteLength(body) - Buffer.byteLength(prefix) - 1;
  if (remaining < 0) throw new Error(`Source body exceeds target: ${size}/${language}/${index}`);
  const pad = sha256(Buffer.from(`${seed}\0${size}\0${language}\0${index}\0pad`, "utf8"));
  const bytes = Buffer.from(body + prefix + pad.repeat(Math.ceil(remaining / pad.length)).slice(0, remaining) + "\n", "utf8");
  if (bytes.length !== target) throw new Error("Source length mismatch");
  return bytes;
}

function main(args) {
  if (args.length !== 2 || args[0] !== "--out" || !args[1])
    throw new Error("Usage: node tools/synthetic-cohorts/generate.mjs --out <nonexistent-directory>");
  const out = resolve(args[1]);
  if (existsSync(out)) throw new Error(`Output already exists: ${out}`);
  mkdirSync(out);
  try {
    const files = [];
    const totals = {};
    for (const [size, count, bytes] of sizes) {
      totals[size] = { files: count * languages.length, sourceBytes: bytes };
      for (const [language, extension] of languages) {
        const ordinal = languages.findIndex(([name]) => name === language);
        const languageBytes = Math.floor(bytes / languages.length) + (ordinal < bytes % languages.length ? 1 : 0);
        const folder = join(out, size, language);
        mkdirSync(folder, { recursive: true });
        for (let index = 0; index < count; index++) {
          const target = Math.floor(languageBytes / count) + (index < languageBytes % count ? 1 : 0);
          const path = `${size}/${language}/C${size}${String(index).padStart(4, "0")}.${extension}`;
          const content = source(size, language, index, target);
          writeFileSync(join(folder, path.split("/").at(-1)), content, { flag: "wx" });
          files.push({ path, sourceBytes: content.length, sha256: sha256(content) });
        }
      }
    }
    files.sort((a, b) => a.path < b.path ? -1 : a.path > b.path ? 1 : 0);
    writeFileSync(join(out, "manifest.json"), JSON.stringify({ version: 1, seed, hashAlgorithm: "sha256", totals, files }, null, 2) + "\n", { flag: "wx" });
  } catch (error) {
    rmSync(out, { recursive: true, force: true });
    throw error;
  }
}

try {
  main(process.argv.slice(2));
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
