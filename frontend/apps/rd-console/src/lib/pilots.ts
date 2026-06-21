/**
 * Pilot presentation + form helpers (issue #74).
 *
 * Pure mappers shared by the directory list and the add/edit form: the selectable VTX types, the
 * Badge tone for a {@link VtxType}, and the **clear-via-null** diff that turns a form's working
 * values into an {@link UpdatePilotRequest} (omit unchanged, `null` to clear, value to set). No I/O
 * — the {@link Session} owns the protocol calls; these only shape data for the UI and the wire.
 */
import type { CreatePilotRequest, Pilot, UpdatePilotRequest, VtxType } from '@gridfpv/types';

/**
 * The selectable VTX types in the form, in display order. A pilot carries a **set** of these (every
 * FPV pilot flies some video system, and many run more than one), so the form toggles them on/off
 * rather than picking one — there is no "None". `Other` is the catch-all.
 */
export const VTX_TYPES: readonly VtxType[] = ['Analog', 'HDZero', 'DJI', 'Walksnail', 'Other'];

/** The Badge `tone` for each VTX type — a quiet, consistent palette across the directory. */
const VTX_TONES: Record<VtxType, 'accent' | 'info' | 'success' | 'warn' | 'neutral'> = {
  Analog: 'warn',
  HDZero: 'info',
  DJI: 'accent',
  Walksnail: 'success',
  Other: 'neutral'
};

/** The Badge `tone` for a VTX type. */
export function vtxTone(vtx: VtxType): 'accent' | 'info' | 'success' | 'warn' | 'neutral' {
  return VTX_TONES[vtx];
}

/** Dedup a VTX list and return it in the canonical {@link VTX_TYPES} order (stable, set-like). */
export function normalizeVtxTypes(types: readonly VtxType[]): VtxType[] {
  return VTX_TYPES.filter((kind) => types.includes(kind));
}

/** Whether two VTX sets are equal as sets (order-independent). */
function sameVtxSet(a: readonly VtxType[], b: readonly VtxType[]): boolean {
  if (a.length !== b.length) return false;
  return a.every((kind) => b.includes(kind));
}

/**
 * The editable shape the form holds while open — every text field a string (the form binds plain
 * inputs) plus the attributes bag and the `vtx_types` **set** (the toggle chips). The form diffs this
 * against the pilot being edited to build the {@link UpdatePilotRequest}, or maps it straight to a
 * {@link CreatePilotRequest}.
 */
export interface PilotFormValues {
  callsign: string;
  name: string;
  phonetic: string;
  team: string;
  vtx_types: VtxType[];
  color: string;
  country: string;
  multigp_id: string;
  velocidrone_id: string;
  attributes: Record<string, string>;
}

/** Blank form values for the **Add** flow. */
export function emptyForm(): PilotFormValues {
  return {
    callsign: '',
    name: '',
    phonetic: '',
    team: '',
    vtx_types: [],
    color: '',
    country: '',
    multigp_id: '',
    velocidrone_id: '',
    attributes: {}
  };
}

/** Seed the form from an existing pilot for the **Edit** flow. */
export function formFromPilot(p: Pilot): PilotFormValues {
  return {
    callsign: p.callsign,
    name: p.name ?? '',
    phonetic: p.phonetic ?? '',
    team: p.team ?? '',
    vtx_types: normalizeVtxTypes(p.vtx_types ?? []),
    color: p.color ?? '',
    country: p.country ?? '',
    multigp_id: p.multigp_id ?? '',
    velocidrone_id: p.velocidrone_id ?? '',
    attributes: { ...p.attributes }
  };
}

/** Toggle a VTX type in a set — add it if absent, remove it if present (returns a new array). */
export function toggleVtxType(types: readonly VtxType[], vtx: VtxType): VtxType[] {
  return types.includes(vtx)
    ? types.filter((kind) => kind !== vtx)
    : normalizeVtxTypes([...types, vtx]);
}

/** Drop attribute rows with a blank key; trim keys (values are kept verbatim). */
export function cleanAttributes(attrs: Record<string, string>): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(attrs)) {
    const key = k.trim();
    if (key) out[key] = v;
  }
  return out;
}

/**
 * Build the **create** body from the form: the required callsign plus only the optional text fields
 * the user filled (a blank optional is simply omitted — `POST /pilots` treats absent as unset). The
 * attributes bag and the `vtx_types` set are always sent (an empty `{}` / `[]` is fine — both default
 * empty server-side).
 */
export function buildCreateRequest(v: PilotFormValues): CreatePilotRequest {
  const req: CreatePilotRequest = {
    callsign: v.callsign.trim(),
    vtx_types: normalizeVtxTypes(v.vtx_types),
    attributes: cleanAttributes(v.attributes)
  };
  const name = v.name.trim();
  if (name) req.name = name;
  const phonetic = v.phonetic.trim();
  if (phonetic) req.phonetic = phonetic;
  const team = v.team.trim();
  if (team) req.team = team;
  const color = v.color.trim();
  if (color) req.color = color;
  const country = v.country.trim();
  if (country) req.country = country.toUpperCase();
  const multigp = v.multigp_id.trim();
  if (multigp) req.multigp_id = multigp;
  const velocidrone = v.velocidrone_id.trim();
  if (velocidrone) req.velocidrone_id = velocidrone;
  return req;
}

/**
 * Build the **update** body as a minimal three-state diff against the pilot being edited (#74):
 *
 *  - **omit** a field whose form value still matches the pilot → leave it unchanged;
 *  - send a **`null`** when the user emptied a field that had a value → **clear** it (this is what
 *    makes `color`/`country` actually clear — sending `''` would fail the server's `#hex`/2-letter
 *    validation, so a cleared field *must* go as `null`);
 *  - send the **value** when the user changed it → set it.
 *
 * `callsign` is special: a present non-empty value replaces it; a blank one is omitted (the callsign
 * is required and never cleared). `attributes` is a **full replacement** — sent whenever it differs.
 */
export function buildUpdateRequest(prev: Pilot, v: PilotFormValues): UpdatePilotRequest {
  const req: UpdatePilotRequest = {};

  const callsign = v.callsign.trim();
  if (callsign && callsign !== prev.callsign) req.callsign = callsign;

  // A text optional: omit if unchanged, `null` if cleared, value if set.
  const diffText = (next: string, before: string | undefined): string | null | undefined => {
    const trimmed = next.trim();
    const had = before ?? '';
    if (trimmed === had) return undefined; // unchanged
    return trimmed === '' ? null : trimmed; // cleared → null, else set
  };

  const name = diffText(v.name, prev.name);
  if (name !== undefined) req.name = name;
  const phonetic = diffText(v.phonetic, prev.phonetic);
  if (phonetic !== undefined) req.phonetic = phonetic;
  const team = diffText(v.team, prev.team);
  if (team !== undefined) req.team = team;
  const color = diffText(v.color, prev.color);
  if (color !== undefined) req.color = color;
  const multigp = diffText(v.multigp_id, prev.multigp_id);
  if (multigp !== undefined) req.multigp_id = multigp;
  const velocidrone = diffText(v.velocidrone_id, prev.velocidrone_id);
  if (velocidrone !== undefined) req.velocidrone_id = velocidrone;

  // Country: codes are uppercased before comparing/sending; cleared → null.
  const nextCountry = v.country.trim().toUpperCase();
  const prevCountry = prev.country ?? '';
  if (nextCountry !== prevCountry) req.country = nextCountry === '' ? null : nextCountry;

  // VTX: a full-replacement set (like attributes) — send the array whenever it differs as a set;
  // a now-empty set is sent as `[]` (clears it). There is no "None" / null case.
  const nextVtx = normalizeVtxTypes(v.vtx_types);
  if (!sameVtxSet(nextVtx, prev.vtx_types ?? [])) req.vtx_types = nextVtx;

  // Attributes: a full-replacement map — send it whenever the cleaned bag differs.
  const nextAttrs = cleanAttributes(v.attributes);
  if (!shallowEqualRecord(nextAttrs, prev.attributes)) req.attributes = nextAttrs;

  return req;
}

/** Whether two `string → string` records have identical keys and values. */
function shallowEqualRecord(a: Record<string, string>, b: Record<string, string>): boolean {
  const ak = Object.keys(a);
  const bk = Object.keys(b);
  if (ak.length !== bk.length) return false;
  return ak.every((k) => a[k] === b[k]);
}
