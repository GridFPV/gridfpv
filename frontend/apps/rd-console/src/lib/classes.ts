/**
 * Class presentation + form helpers (issue #84).
 *
 * Pure mappers shared by the directory list and the add/edit form: the selectable class sources,
 * the Badge tone for a {@link ClassSource}, the **clear-via-null** diff that turns a form's working
 * values into an {@link UpdateClassRequest} (omit unchanged, `null` to clear, value to set), and a
 * static **MultiGP quick-pick** list that pre-fills the create form with `source = MultiGP` plus a
 * reference URL for one of the standard MultiGP classes. No I/O — the {@link Session} owns the
 * protocol calls; these only shape data for the UI and the wire. Mirrors `lib/pilots.ts`.
 */
import type { Class, ClassSource, CreateClassRequest, UpdateClassRequest } from '@gridfpv/types';

/**
 * The selectable class sources in the form, in display order. A class records **one** provenance,
 * so the form picks one (a `<select>`). `Custom` is the default (a class the RD typed in); `Other`
 * is the catch-all.
 */
export const CLASS_SOURCES: readonly ClassSource[] = ['MultiGP', 'Custom', 'Other'];

/** The Badge `tone` for each class source — a quiet, consistent palette across the directory. */
const SOURCE_TONES: Record<ClassSource, 'accent' | 'info' | 'neutral'> = {
  MultiGP: 'accent',
  Custom: 'info',
  Other: 'neutral'
};

/** The Badge `tone` for a class source. */
export function sourceTone(source: ClassSource): 'accent' | 'info' | 'neutral' {
  return SOURCE_TONES[source];
}

/**
 * The editable shape the form holds while open — every text field a string (the form binds plain
 * inputs) plus the `source` provenance. The form diffs this against the class being edited to build
 * the {@link UpdateClassRequest}, or maps it straight to a {@link CreateClassRequest}.
 */
export interface ClassFormValues {
  name: string;
  source: ClassSource;
  reference: string;
  description: string;
}

/** Blank form values for the **Add** flow — defaults to the `Custom` provenance. */
export function emptyForm(): ClassFormValues {
  return { name: '', source: 'Custom', reference: '', description: '' };
}

/** Seed the form from an existing class for the **Edit** flow. */
export function formFromClass(c: Class): ClassFormValues {
  return {
    name: c.name,
    source: c.source,
    reference: c.reference ?? '',
    description: c.description ?? ''
  };
}

/**
 * Build the **create** body from the form: the required name + source plus only the optional text
 * fields the user filled (a blank optional is simply omitted — `POST /classes` treats absent as
 * unset).
 */
export function buildCreateRequest(v: ClassFormValues): CreateClassRequest {
  const req: CreateClassRequest = {
    name: v.name.trim(),
    source: v.source
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
 *  - `source` — sent only when it changed.
 *  - `reference` / `description` — the **clear-via-null** three-state: omit if unchanged, `null` if
 *    the user emptied a field that had a value (clears it), value if set. Mirrors
 *    `buildUpdateRequest` in `lib/pilots.ts`.
 */
export function buildUpdateRequest(prev: Class, v: ClassFormValues): UpdateClassRequest {
  const req: UpdateClassRequest = {};

  const name = v.name.trim();
  if (name && name !== prev.name) req.name = name;

  if (v.source !== prev.source) req.source = v.source;

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

/**
 * One entry in the **MultiGP quick-pick**: a standard MultiGP class the RD can add with one tap,
 * which pre-fills the create form with `source = MultiGP`, the class name, and a reference URL into
 * the MultiGP rules so the provenance is recorded.
 */
export interface MultiGpClassPreset {
  name: string;
  reference: string;
}

/**
 * The seven standard MultiGP classes (the GQ / spec classes). Each carries a reference URL into the
 * MultiGP class rules so a quick-picked class records where it came from. Selecting one pre-fills
 * the create form (`source = MultiGP`, `name`, `reference`); the RD can still tweak before saving.
 */
export const MULTIGP_CLASSES: readonly MultiGpClassPreset[] = [
  { name: 'Spec', reference: 'https://www.multigp.com/multigp-drone-race-classes/' },
  { name: 'Pro Spec', reference: 'https://www.multigp.com/multigp-drone-race-classes/' },
  { name: 'Freedom Spec', reference: 'https://www.multigp.com/multigp-drone-race-classes/' },
  { name: 'Street League', reference: 'https://www.multigp.com/multigp-drone-race-classes/' },
  { name: 'Open', reference: 'https://www.multigp.com/multigp-drone-race-classes/' },
  { name: 'Tiny Whoop', reference: 'https://www.multigp.com/multigp-drone-race-classes/' },
  { name: 'Tiny Trainer', reference: 'https://www.multigp.com/multigp-drone-race-classes/' }
];

/** Build pre-filled form values for a MultiGP quick-pick preset (the create form seeds from this). */
export function formFromMultiGp(preset: MultiGpClassPreset): ClassFormValues {
  return { name: preset.name, source: 'MultiGP', reference: preset.reference, description: '' };
}
