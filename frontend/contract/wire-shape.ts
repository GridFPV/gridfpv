/**
 * Assert a wire body against the **generated** ts-rs binding for its type (#410).
 *
 * ── Why this exists ─────────────────────────────────────────────────────────
 * The bug this suite is guarding against was a *hand-declared* wire type: an interface
 * written from memory while the backend slice was unmerged, whose every field name turned
 * out to differ from the real one. `tsc` cannot tell a fabricated interface from a real
 * one, and unit tests built on fixtures shaped like the fabrication agree with it happily.
 *
 * So a contract test that spelled the expected body out by hand would be the *same failure
 * mode one level down* — a second hand-written copy of the shape, free to drift from the
 * Rust type in exactly the same way. Instead this reads `bindings/<Type>.ts` — the file
 * `cargo xtask ci` already keeps byte-identical to the Rust struct (`gen_check`) — and
 * derives the expectation from it. Rename a field in Rust and the expectation moves with
 * it; the assertion has no opinion of its own to be wrong.
 *
 * ── What it checks ──────────────────────────────────────────────────────────
 * For an object type: every non-optional field is present, no field is present that the
 * binding does not declare (a wire key the frontend cannot see is drift too), and every
 * present value matches its declared type — recursing through `Array<…>`, unions, inline
 * objects and named sibling bindings (`TimerId`, `NodeSignal`, …).
 *
 * `#[ts(optional)]` fields (`rssi?: number`) may be absent or `undefined`; that is the
 * contract, since the Rust side skips serializing `None`.
 */
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

/** The repo-root `bindings/` dir — the same physical location `@bindings/*` aliases to. */
const BINDINGS_DIR = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..', 'bindings');

/** One declared field of a generated object type. */
export interface FieldSpec {
  /** The field name exactly as it appears on the wire. */
  name: string;
  /** `true` for a `name?: T` field — the Rust side omits it when `None`. */
  optional: boolean;
  /** The declared type expression, verbatim (`Array<NodeSignal>`, `number | null`, …). */
  type: string;
}

/** The type expression each binding declares, parsed once per process. */
const bodyCache = new Map<string, string>();

/** Strip ts-rs's doc comments and the generated-file banner. */
function stripComments(src: string): string {
  return src.replace(/\/\*[\s\S]*?\*\//g, ' ').replace(/^[ \t]*\/\/.*$/gm, '');
}

/** The characters that open/close a nesting level in a TS type expression. */
const OPENERS = '{[(<';
const CLOSERS = '}])>';

/**
 * Walk `src` from `start`, calling `at(index, char)` for every character that sits at
 * nesting depth 0 and outside a string literal. Returns the index the walk stopped at.
 */
function scanTopLevel(src: string, stop: (index: number, char: string) => boolean): number {
  let depth = 0;
  let quote: string | undefined;
  for (let i = 0; i < src.length; i += 1) {
    const char = src[i];
    if (quote !== undefined) {
      if (char === '\\') i += 1;
      else if (char === quote) quote = undefined;
      continue;
    }
    if (char === '"' || char === "'" || char === '`') {
      quote = char;
      continue;
    }
    if (OPENERS.includes(char)) depth += 1;
    else if (CLOSERS.includes(char)) depth -= 1;
    else if (depth === 0 && stop(i, char)) return i;
  }
  return -1;
}

/** Split `src` on top-level occurrences of `separator`. */
function splitTopLevel(src: string, separator: string): string[] {
  const parts: string[] = [];
  let from = 0;
  let offset = 0;
  for (;;) {
    const rest = src.slice(offset);
    const at = scanTopLevel(rest, (_i, char) => char === separator);
    if (at < 0) break;
    parts.push(src.slice(from, offset + at));
    from = offset + at + 1;
    offset = from;
  }
  parts.push(src.slice(from));
  return parts;
}

/** The type expression `bindings/<name>.ts` declares, comments stripped. */
function bindingBody(name: string): string {
  const cached = bodyCache.get(name);
  if (cached !== undefined) return cached;
  let raw: string;
  try {
    raw = readFileSync(join(BINDINGS_DIR, `${name}.ts`), 'utf8');
  } catch {
    throw new Error(
      `no generated binding for \`${name}\` (looked for bindings/${name}.ts) — ` +
        `regenerate with \`cargo xtask gen\``
    );
  }
  const src = stripComments(raw);
  const marker = `export type ${name} =`;
  const at = src.indexOf(marker);
  if (at < 0) throw new Error(`bindings/${name}.ts does not declare \`export type ${name}\``);
  const rest = src.slice(at + marker.length);
  const end = scanTopLevel(rest, (_i, char) => char === ';');
  const body = (end < 0 ? rest : rest.slice(0, end)).trim();
  bodyCache.set(name, body);
  return body;
}

/** Parse an object type expression (`{ a: number, b?: string, }`) into its fields. */
function parseFields(body: string, label: string): FieldSpec[] {
  const inner = body.slice(1, -1);
  return splitTopLevel(inner, ',')
    .map((part) => part.trim())
    .filter((part) => part.length > 0)
    .map((part) => {
      const colon = scanTopLevel(part, (_i, char) => char === ':');
      if (colon < 0) throw new Error(`cannot parse field \`${part}\` of ${label}`);
      let name = part.slice(0, colon).trim();
      const optional = name.endsWith('?');
      if (optional) name = name.slice(0, -1).trim();
      if (name.startsWith('"') || name.startsWith("'")) name = name.slice(1, -1);
      return { name, optional, type: part.slice(colon + 1).trim() };
    });
}

/**
 * The fields the generated binding declares for an **object** type — the wire's own field
 * list, for a test that wants to assert against it directly.
 */
export function bindingFields(typeName: string): FieldSpec[] {
  const body = bindingBody(typeName);
  if (!body.startsWith('{') || !body.endsWith('}'))
    throw new Error(`bindings/${typeName}.ts is not an object type: ${body}`);
  return parseFields(body, typeName);
}

/** A JSON-ish object (not an array, not `null`). */
function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** A short, readable rendering of an actual value for a mismatch message. */
function show(value: unknown): string {
  if (typeof value === 'string') return JSON.stringify(value);
  if (Array.isArray(value)) return `array(${value.length})`;
  if (value === null) return 'null';
  if (isPlainObject(value)) return `{ ${Object.keys(value).join(', ')} }`;
  return String(value);
}

/** Everything about `value` that disagrees with the type expression `type`. */
function problemsFor(value: unknown, type: string, path: string, seen: string[]): string[] {
  let expr = type.trim();
  // Unwrap a fully-parenthesised expression (`(A | B)[]` keeps its parens, `(A | B)` loses them).
  while (expr.startsWith('(') && expr.endsWith(')')) {
    const inner = expr.slice(1, -1);
    // Only unwrap when that `)` really closed the leading `(` — i.e. nothing at depth 0 inside.
    if (scanTopLevel(inner, (_i, char) => char === ')') >= 0) break;
    expr = inner.trim();
  }

  // A union passes if any branch does; report the whole union when none do.
  const branches = splitTopLevel(expr, '|').map((b) => b.trim());
  if (branches.length > 1) {
    const ok = branches.some((branch) => problemsFor(value, branch, path, seen).length === 0);
    return ok ? [] : [`${path}: matches no branch of ${expr.replace(/\s+/g, ' ')}`];
  }

  const mismatch = (): string[] => [
    `${path}: expected ${expr.replace(/\s+/g, ' ')}, got ${show(value)}`
  ];

  switch (expr) {
    case 'number':
      return typeof value === 'number' && Number.isFinite(value) ? [] : mismatch();
    case 'string':
      return typeof value === 'string' ? [] : mismatch();
    case 'boolean':
      return typeof value === 'boolean' ? [] : mismatch();
    case 'bigint':
      // Serialized as a JSON number by every `#[ts(as = …)]`-free i64 on the wire; the client
      // is what turns it into a `bigint`, so accept either here.
      return typeof value === 'bigint' || typeof value === 'number' ? [] : mismatch();
    case 'null':
      return value === null ? [] : mismatch();
    case 'undefined':
      return value === undefined ? [] : mismatch();
    case 'unknown':
    case 'any':
      return [];
    default:
      break;
  }

  // A string/number literal type (`"Mock"`, `4`).
  if (/^["'].*["']$/.test(expr)) return value === expr.slice(1, -1) ? [] : mismatch();
  if (/^-?\d+(\.\d+)?$/.test(expr)) return value === Number(expr) ? [] : mismatch();

  // `Array<T>` / `T[]`.
  const arrayOf = /^Array<([\s\S]+)>$/.exec(expr)?.[1] ?? /^([\s\S]+)\[\]$/.exec(expr)?.[1];
  if (arrayOf !== undefined) {
    if (!Array.isArray(value)) return mismatch();
    return value.flatMap((item, index) => problemsFor(item, arrayOf, `${path}[${index}]`, seen));
  }

  // `Record<K, V>` — ts-rs's rendering of a map.
  const record = /^Record<([\s\S]+)>$/.exec(expr)?.[1];
  if (record !== undefined) {
    const [, valueType] = splitTopLevel(record, ',');
    if (!isPlainObject(value)) return mismatch();
    return Object.entries(value).flatMap(([key, entry]) =>
      problemsFor(entry, (valueType ?? 'unknown').trim(), `${path}.${key}`, seen)
    );
  }

  // An inline or named object type.
  if (expr.startsWith('{') && expr.endsWith('}'))
    return objectProblems(value, parseFields(expr, path), path, seen);

  // A named sibling binding: resolve it through `bindings/<Name>.ts`.
  if (/^[A-Za-z_][A-Za-z0-9_]*$/.test(expr)) {
    if (seen.includes(expr)) return []; // recursive type — one pass through is enough
    return problemsFor(value, bindingBody(expr), path, [...seen, expr]);
  }

  throw new Error(`wire-shape: unsupported type expression \`${expr}\` at ${path}`);
}

/** Everything about `value` that disagrees with a declared field list. */
function objectProblems(
  value: unknown,
  fields: FieldSpec[],
  path: string,
  seen: string[]
): string[] {
  if (!isPlainObject(value)) return [`${path}: expected an object, got ${show(value)}`];
  const problems: string[] = [];
  for (const field of fields) {
    const at = path === '' ? field.name : `${path}.${field.name}`;
    const present = field.name in value && value[field.name] !== undefined;
    if (!present) {
      if (!field.optional) problems.push(`${at}: missing (declared \`${field.type}\`)`);
      continue;
    }
    problems.push(...problemsFor(value[field.name], field.type, at, seen));
  }
  const declared = new Set(fields.map((f) => f.name));
  for (const key of Object.keys(value))
    if (!declared.has(key)) problems.push(`${path}.${key}: on the wire, not in the binding`);
  return problems;
}

/**
 * Everything about `value` that disagrees with `bindings/<typeName>.ts` — empty when the
 * body is exactly the shape the generated binding declares.
 *
 * Returned rather than thrown so a test can `expect(wireShapeProblems(...)).toEqual([])`
 * and get every mismatch at once (a renamed type shows up as *all* its fields, which is
 * the signal that tells "one field moved" apart from "this is a different type").
 */
export function wireShapeProblems(value: unknown, typeName: string): string[] {
  return problemsFor(value, typeName, typeName, []);
}
