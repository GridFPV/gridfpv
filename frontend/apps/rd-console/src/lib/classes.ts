/**
 * Class presentation + form helpers (issue #84).
 *
 * Pure mappers shared by the directory list and the add/edit form: the Badge tone for a
 * {@link ClassSource}, whether a {@link Class} is a read-only **built-in**, and the
 * **clear-via-null** diff that turns a form's working values into an {@link UpdateClassRequest}
 * (omit unchanged, `null` to clear, value to set). No I/O — the {@link Session} owns the protocol
 * calls; these only shape data for the UI and the wire. Mirrors `lib/pilots.ts`.
 *
 * Users only ever create **`Custom`** classes (the create form fixes the source to Custom). The
 * canonical org classes (MultiGP, Five33, FreedomSpec, StreetLeague, UDL) ship as locked,
 * fixed-id **built-ins** seeded by the Director — so the old "Add from MultiGP" quick-pick is gone.
 */
import type { Class, ClassSource, CreateClassRequest, UpdateClassRequest } from '@gridfpv/types';

/** The Badge tones used for class sources (a subset of the Badge component's `tone` union). */
type SourceTone = 'accent' | 'info' | 'success' | 'warn' | 'danger' | 'violet' | 'neutral';

/**
 * The Badge `tone` for each class source — a distinct tone per org so the built-in classes read at
 * a glance. `Custom` (the classes users create) gets its own **violet** tone — a "your own /
 * user-defined" color that reads apart from the org tones (green/blue/amber/red) without clashing.
 * `Other` stays neutral.
 */
const SOURCE_TONES: Record<ClassSource, SourceTone> = {
  MultiGP: 'accent',
  Five33: 'success',
  FreedomSpec: 'info',
  StreetLeague: 'warn',
  UDL: 'danger',
  Custom: 'violet',
  Other: 'neutral'
};

/** The Badge `tone` for a class source. */
export function sourceTone(source: ClassSource): SourceTone {
  return SOURCE_TONES[source];
}

/**
 * Whether a class is a read-only **built-in** (issue #84): a canonical, fixed-id class the Director
 * seeds and that cannot be edited or removed. The server flags these with `builtin: true`.
 */
export function isBuiltin(c: Class): boolean {
  return c.builtin === true;
}

/**
 * The editable shape the form holds while open — every text field a string (the form binds plain
 * inputs). New classes are always `Custom`, so the create form carries no source picker; the form
 * diffs this against the class being edited to build the {@link UpdateClassRequest}, or maps it
 * straight to a {@link CreateClassRequest} (with `source = Custom`).
 */
export interface ClassFormValues {
  name: string;
  reference: string;
  description: string;
}

/** Blank form values for the **Add** flow. */
export function emptyForm(): ClassFormValues {
  return { name: '', reference: '', description: '' };
}

/** Seed the form from an existing class for the **Edit** flow (Custom classes only). */
export function formFromClass(c: Class): ClassFormValues {
  return {
    name: c.name,
    reference: c.reference ?? '',
    description: c.description ?? ''
  };
}

/**
 * Build the **create** body from the form: the required name plus only the optional text fields the
 * user filled (a blank optional is simply omitted). The source is always `Custom` — users only ever
 * create custom classes; the canonical org classes ship as built-ins.
 */
export function buildCreateRequest(v: ClassFormValues): CreateClassRequest {
  const req: CreateClassRequest = {
    name: v.name.trim(),
    source: 'Custom'
  };
  const reference = v.reference.trim();
  if (reference) req.reference = reference;
  const description = v.description.trim();
  if (description) req.description = description;
  return req;
}

/**
 * Build the **update** body as a minimal diff against the class being edited (#84):
 *
 *  - `name` — a present non-empty value that differs replaces it; a blank or unchanged one is
 *    omitted (the name is required and never cleared).
 *  - `reference` / `description` — the **clear-via-null** three-state: omit if unchanged, `null` if
 *    the user emptied a field that had a value (clears it), value if set. Mirrors
 *    `buildUpdateRequest` in `lib/pilots.ts`.
 *
 * Built-in classes are read-only and never reach this path (the UI offers no Edit on them).
 */
export function buildUpdateRequest(prev: Class, v: ClassFormValues): UpdateClassRequest {
  const req: UpdateClassRequest = {};

  const name = v.name.trim();
  if (name && name !== prev.name) req.name = name;

  // A text optional: omit if unchanged, `null` if cleared, value if set.
  const diffText = (next: string, before: string | undefined): string | null | undefined => {
    const trimmed = next.trim();
    const had = before ?? '';
    if (trimmed === had) return undefined; // unchanged
    return trimmed === '' ? null : trimmed; // cleared → null, else set
  };

  const reference = diffText(v.reference, prev.reference);
  if (reference !== undefined) req.reference = reference;
  const description = diffText(v.description, prev.description);
  if (description !== undefined) req.description = description;

  return req;
}
