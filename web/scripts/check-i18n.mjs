// Checks the rules of docs/PLAN.md, section 6, on the translation files:
//
//   1. every language of the language list has a folder, and every folder is in the list;
//   2. every resource file (translation.json, email.json) has exactly the same keys in
//      every language, plural forms aside;
//   3. each language has the plural forms its grammar needs (English: one/other; French:
//      one/many/other), and each text uses the same {{placeholders}} as the English one;
//   4. every key the code uses exists, and every key that exists is used.
//
// Run with `pnpm check:i18n`; it is part of `pnpm test`.
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";

const root = new URL("..", import.meta.url).pathname;
const localesDir = join(root, "src/locales");
const DEFAULT = "en";
const PLURAL_SUFFIX = /_(zero|one|two|few|many|other)$/;
const errors = [];
const fail = (message) => errors.push(message);

// --- 1. languages
const listed = [...readFileSync(join(root, "src/i18n/languages.ts"), "utf8").matchAll(/code:\s*"([a-z-]+)"/g)].map((m) => m[1]);
const folders = readdirSync(localesDir).filter((name) => statSync(join(localesDir, name)).isDirectory());
for (const code of listed) if (!folders.includes(code)) fail(`language "${code}" is listed but has no folder in src/locales/`);
for (const code of folders) if (!listed.includes(code)) fail(`src/locales/${code}/ exists but "${code}" is not in src/i18n/languages.ts`);
if (!listed.includes(DEFAULT)) fail(`the default language "${DEFAULT}" must be in the language list`);

// --- helpers
function flatten(value, prefix = "", out = new Map()) {
  for (const [key, child] of Object.entries(value)) {
    const path = prefix ? `${prefix}.${key}` : key;
    if (child !== null && typeof child === "object") flatten(child, path, out);
    else if (typeof child === "string") out.set(path, child);
    else fail(`${path}: values must be strings`);
  }
  return out;
}
const placeholders = (text) => [...text.matchAll(/{{\s*([a-zA-Z_]+)\s*}}/g)].map((m) => m[1]).sort().join(",");
const base = (key) => key.replace(PLURAL_SUFFIX, "");

// --- 2 and 3. same keys, plural forms, placeholders
const files = readdirSync(join(localesDir, DEFAULT)).filter((name) => name.endsWith(".json"));
const catalogs = {};
for (const file of files) {
  for (const code of folders) {
    let texts;
    try {
      texts = flatten(JSON.parse(readFileSync(join(localesDir, code, file), "utf8")));
    } catch (e) {
      fail(`src/locales/${code}/${file}: ${e.message}`);
      continue;
    }
    (catalogs[file] ??= {})[code] = texts;
  }
  const reference = catalogs[file][DEFAULT];
  const referenceBases = new Set([...reference.keys()].map(base));
  for (const code of folders) {
    const texts = catalogs[file][code];
    if (!texts) continue;
    const bases = new Set([...texts.keys()].map(base));
    for (const key of referenceBases) if (!bases.has(key)) fail(`${code}/${file}: missing key "${key}"`);
    for (const key of bases) if (!referenceBases.has(key)) fail(`${code}/${file}: key "${key}" does not exist in ${DEFAULT}`);

    const categories = new Intl.PluralRules(code).resolvedOptions().pluralCategories;
    const pluralBases = new Set([...texts.keys()].filter((k) => PLURAL_SUFFIX.test(k)).map(base));
    for (const key of pluralBases) {
      for (const category of categories) {
        if (!texts.has(`${key}_${category}`)) fail(`${code}/${file}: "${key}" lacks the plural form "_${category}"`);
      }
    }
    for (const [key, text] of texts) {
      if (text.trim() === "") fail(`${code}/${file}: "${key}" is empty`);
      const referenceText = reference.get(key) ?? reference.get(`${base(key)}_other`);
      if (referenceText !== undefined && placeholders(text) !== placeholders(referenceText)) {
        fail(`${code}/${file}: "${key}" uses {{${placeholders(text)}}} but ${DEFAULT} uses {{${placeholders(referenceText)}}}`);
      }
    }
  }
}

// --- 4. keys used by the code
function sources(dir, out = []) {
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) {
      if (name !== "locales") sources(path, out);
    } else if (/\.tsx?$/.test(name)) out.push(path);
  }
  return out;
}
const known = new Set([...(catalogs["translation.json"]?.[DEFAULT]?.keys() ?? [])].map(base));
const used = new Set();
const usedPrefixes = new Set();
for (const file of sources(join(root, "src"))) {
  const code = readFileSync(file, "utf8");
  for (const match of code.matchAll(/\b(?:t|exists)\(\s*"([A-Za-z0-9_.]+)"/g)) {
    used.add(match[1]);
    if (!known.has(match[1])) fail(`${relative(root, file)}: key "${match[1]}" is not in ${DEFAULT}/translation.json`);
  }
  // Keys built at run time, such as t(`status.${status}`): the fixed part must lead somewhere.
  for (const match of code.matchAll(/\b(?:t|exists)\(\s*`([A-Za-z0-9_.]*)\$\{[^}]+\}([A-Za-z0-9_.]*)`/g)) {
    const [, prefix, suffix] = match;
    const candidates = [...known].filter((key) => key.startsWith(prefix) && key.endsWith(suffix));
    if (candidates.length === 0) fail(`${relative(root, file)}: no key matches \`${prefix}…${suffix}\``);
    usedPrefixes.add(`${prefix}|${suffix}`);
    for (const key of candidates) used.add(key);
  }
}
// API error codes are translated through `errors.<code>`; the server's own test checks
// that each code has a text (crates/server/src/error.rs).
for (const key of known) {
  if (!used.has(key) && !key.startsWith("errors.")) fail(`${DEFAULT}/translation.json: key "${key}" is not used by the code`);
}

if (errors.length > 0) {
  console.error(`i18n check failed (${errors.length}):`);
  for (const message of errors) console.error(`  - ${message}`);
  process.exit(1);
}
const count = catalogs["translation.json"][DEFAULT].size;
console.log(`i18n check passed: ${folders.length} languages (${folders.join(", ")}), ${files.length} files each, ${count} UI texts in ${DEFAULT}.`);
