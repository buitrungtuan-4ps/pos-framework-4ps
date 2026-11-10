// The `receipt.templates` setting as the console edits it (ADR-0207): a base template and at most one
// per sales channel, each a few translation keys above and below the receipt's legal body, and the
// optional sections on or off.
//
// The setting's value reaches the console as plain JSON (the register's `SETTING_KIND_RECEIPT_TEMPLATES`
// carries no schema of its own), so its shape is mirrored here from `pos_proto::receipt`, and the
// register's own check is the authority: what this file refuses, the cloud refuses too. A template
// field this console does not edit (the custom fields that print, the documents that count as a
// person's copy) is carried through every edit unchanged.

/** One template, as `pos_proto::receipt::ReceiptTemplate` holds it. */
export type ReceiptTemplate = {
  readonly header_keys?: readonly string[];
  readonly footer_keys?: readonly string[];
  readonly sections?: Readonly<Partial<Record<ReceiptSectionField, boolean>>>;
  readonly [field: string]: unknown;
};

/** The setting's value, as `pos_proto::receipt::ReceiptTemplates` holds it. */
export type ReceiptTemplates = {
  readonly base?: ReceiptTemplate;
  readonly channels?: Readonly<Record<string, ReceiptTemplate>>;
  readonly [field: string]: unknown;
};

/** The template being edited: the base, or one channel's. */
export type TemplateSlot = "base" | string;

/** At most this many header keys and this many footer keys (`pos_proto::receipt::KEYS_MAX`). */
export const KEYS_MAX = 4;

/** At most this many per-channel templates (`CHANNEL_TEMPLATES_MAX`). */
export const CHANNEL_TEMPLATES_MAX = 16;

/** The switches of `ReceiptSections`, by the `RECEIPT_SECTION_*` token a country names it with. */
export const SECTIONS = [
  { field: "modifiers", token: "RECEIPT_SECTION_MODIFIERS" },
  { field: "payments", token: "RECEIPT_SECTION_PAYMENTS" },
  { field: "change_line", token: "RECEIPT_SECTION_CHANGE_LINE" },
  { field: "second_language", token: "RECEIPT_SECTION_SECOND_LANGUAGE" },
] as const;

/** A section switch's field name. */
export type ReceiptSectionField = (typeof SECTIONS)[number]["field"];

const isObject = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);

/** The value as templates: anything that is not an object reads as none written. */
export function readTemplates(value: unknown): ReceiptTemplates {
  return isObject(value) ? (value as ReceiptTemplates) : {};
}

/** The template in `slot`, or none. */
export function templateAt(templates: ReceiptTemplates, slot: TemplateSlot): ReceiptTemplate | undefined {
  return slot === "base" ? templates.base : templates.channels?.[slot];
}

/** `templates` with `slot` replaced by `template`, or removed when `template` is `undefined`. */
export function withTemplate(
  templates: ReceiptTemplates,
  slot: TemplateSlot,
  template: ReceiptTemplate | undefined,
): ReceiptTemplates {
  if (slot === "base") {
    const { base: _dropped, ...rest } = templates;
    return template === undefined ? rest : { ...rest, base: template };
  }
  const { [slot]: _gone, ...channels } = templates.channels ?? {};
  const next = template === undefined ? channels : { ...channels, [slot]: template };
  const { channels: _old, ...rest } = templates;
  return Object.keys(next).length === 0 ? rest : { ...rest, channels: next };
}

/** Whether a section prints under `template`: on unless it is turned off, as on the edge. */
export function sectionOn(template: ReceiptTemplate | undefined, field: ReceiptSectionField): boolean {
  return template?.sections?.[field] !== false;
}

/** `template` with `field` turned on or off. On is the default, so it is left off the wire. */
export function withSection(
  template: ReceiptTemplate,
  field: ReceiptSectionField,
  on: boolean,
): ReceiptTemplate {
  const { [field]: _old, ...others } = template.sections ?? {};
  const sections = on ? others : { ...others, [field]: false };
  const { sections: _previous, ...rest } = template;
  return Object.keys(sections).length === 0 ? rest : { ...rest, sections };
}

/**
 * `template` with its header or footer keys set as typed, so a field being typed in keeps its place.
 * A blank key is a fault the editor says, never one it drops by itself; none at all leaves the list
 * off the wire.
 */
export function withKeys(
  template: ReceiptTemplate,
  part: "header_keys" | "footer_keys",
  keys: readonly string[],
): ReceiptTemplate {
  const { [part]: _old, ...rest } = template;
  return keys.length === 0 ? rest : { ...rest, [part]: keys };
}

/** Why a template cannot be written, before the cloud is asked: the first fault, or `null`. */
export type TemplateFault =
  | { readonly kind: "too_many_keys"; readonly part: "header_keys" | "footer_keys" }
  | { readonly kind: "blank_key"; readonly part: "header_keys" | "footer_keys" }
  | { readonly kind: "key_without_english"; readonly key: string }
  | { readonly kind: "section_required"; readonly field: ReceiptSectionField };

/**
 * The first fault of `template`: too many keys, a blank key, a key whose English text `hasEnglish` does not know
 * (when the grid has been read), or a section `required` names turned off.
 */
export function templateFault(
  template: ReceiptTemplate,
  hasEnglish: ((key: string) => boolean) | null,
  required: ReadonlySet<string>,
): TemplateFault | null {
  for (const part of ["header_keys", "footer_keys"] as const) {
    const keys = template[part] ?? [];
    if (keys.length > KEYS_MAX) {
      return { kind: "too_many_keys", part };
    }
    for (const key of keys) {
      if (key.trim() === "") {
        return { kind: "blank_key", part };
      }
      if (hasEnglish !== null && !hasEnglish(key)) {
        return { kind: "key_without_english", key };
      }
    }
  }
  const off = SECTIONS.find(({ field, token }) => required.has(token) && !sectionOn(template, field));
  return off === undefined ? null : { kind: "section_required", field: off.field };
}

/**
 * Whether `value` is a set of templates the register takes: an object, at most
 * {@link CHANNEL_TEMPLATES_MAX} channels, and every template's keys at most {@link KEYS_MAX} and none
 * blank. Whether each key has English, and the sections a country requires, are the cloud's to
 * answer at write.
 */
export function isTemplates(value: unknown): boolean {
  if (!isObject(value)) {
    return false;
  }
  const templates = value as ReceiptTemplates;
  const channels = templates.channels ?? {};
  if (!isObject(channels) || Object.keys(channels).length > CHANNEL_TEMPLATES_MAX) {
    return false;
  }
  const every = [...(templates.base === undefined ? [] : [templates.base]), ...Object.values(channels)];
  return every.every((template) => isObject(template) && templateFault(template, null, new Set()) === null);
}

/** How many templates the value holds: the base, if any, and each channel's. */
export function templateCount(value: unknown): number {
  const templates = readTemplates(value);
  return (templates.base === undefined ? 0 : 1) + Object.keys(templates.channels ?? {}).length;
}
