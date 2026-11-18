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
import type { Class, CreateClassRequest, UpdateClassRequest } from '@gridfpv/types';

/** The Badge tones used for an org's tag (a subset of the Badge component's `tone` union). */
type OrgTone = 'accent' | 'info' | 'success' | 'warn' | 'danger' | 'violet' | 'neutral';

/** The most characters an org name may have. The Director refuses a longer one. */
export const ORG_MAX = 24;

/**
 * The tag colour of the orgs whose classes come with Grid, so those read at a glance. An org
 * the director typed gets violet: "one of yours".
 */
const ORG_TONES: Record<string, OrgTone> = {
  multigp: 'accent',
  five33: 'success',
  'freedom spec': 'info',
  'street league': 'warn',
  udl: 'danger'
};

/** The Badge `tone` for an org's tag. Letter case and spaces around it do not matter. */
export function orgTone(org: string): OrgTone {
  return ORG_TONES[org.trim().toLowerCase()] ?? 'violet';
}

/** What is wrong with an org as typed, in words, or `undefined` when it can be sent. */
export function orgProblem(text: string): string | undefined {
  return [...text.trim()].length > ORG_MAX
    ? `An org name can have ${ORG_MAX} characters at most.`
    : undefined;
}

/**
 * Whether a class is a read-only **built-in** (issue #84): a canonical, fixed-id class the Director
 * seeds and that cannot be edited or removed. The server flags these with `builtin: true`.
 */
export function isBuiltin(c: Class): boolean {
  return c.builtin === true;
}

/**
 * Whether a class is **hidden / archived** (hide/archive classes): the RD tucked it away, while it
 * stays in the directory and on the Classes page.
 * The server flags these with `hidden: true` (derived from a persisted hidden-set, so a hidden
 * built-in survives a restart). Hiding is a visibility preference, not an edit — so a built-in can
 * be hidden too, even though its edit/delete stay disabled.
 */
export function isHidden(c: Class): boolean {
  return c.hidden === true;
}

/**
 * Partition a directory list into **visible** and **hidden** classes (hide/archive classes),
 * preserving the input (id) order within each group. The Classes page renders both (hidden
 * under a "Hidden" group so it stays manageable / un-hideable).
 */
export function partitionHidden(classes: Class[]): { visible: Class[]; hidden: Class[] } {
  const visible: Class[] = [];
  const hidden: Class[] = [];
  for (const c of classes) (isHidden(c) ? hidden : visible).push(c);
  return { visible, hidden };
}

/**
 * The editable shape the form holds while open — every text field a string (the form binds plain
 * inputs). The form diffs this against the class being edited to build the
 * {@link UpdateClassRequest}, or maps it straight to a {@link CreateClassRequest}.
 */
export interface ClassFormValues {
  name: string;
  /** The org whose class it is, shown as a tag. Empty for none. */
  org: string;
  reference: string;
  description: string;
}

/** Blank form values for the **Add** flow. */
export function emptyForm(): ClassFormValues {
  return { name: '', org: '', reference: '', description: '' };
}

/** Seed the form from an existing class for the **Edit** flow (Custom classes only). */
export function formFromClass(c: Class): ClassFormValues {
  return {
    name: c.name,
    org: c.org ?? '',
    reference: c.reference ?? '',
    description: c.description ?? ''
  };
}

/**
 * Build the **create** body from the form: the required name plus only the optional text fields the
 * user filled (a blank optional is simply omitted).
 */
export function buildCreateRequest(v: ClassFormValues): CreateClassRequest {
  const req: CreateClassRequest = { name: v.name.trim() };
  const org = v.org.trim();
  if (org) req.org = org;
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
 *  - `org` / `reference` / `description` — the **clear-via-null** three-state: omit if unchanged, `null` if
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

  const org = diffText(v.org, prev.org);
  if (org !== undefined) req.org = org;
  const reference = diffText(v.reference, prev.reference);
  if (reference !== undefined) req.reference = reference;
  const description = diffText(v.description, prev.description);
  if (description !== undefined) req.description = description;

  return req;
}
