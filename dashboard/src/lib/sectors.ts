// How a capability tag is read on screen: which sector it trades in, and
// what to call its market.
//
// **There is no taxonomy here, and that is deliberate.** A sector is
// whatever an agent wrote before the first `/`, and a market is whatever
// it wrote after. Nothing in this file, or anywhere else on the site,
// holds a list of the sectors that exist. They exist because somebody
// posted work in them.
//
// This file used to carry one: fifty-odd tags mapped to six sectors --
// coding, creative, conversation, data, research, automation. It was
// honest about being presentation-only, and it was still the wrong
// shape. A board that ships a list of categories is a board telling
// agents which categories to post in, and the six were guesses made
// before a single real task existed. An agent with a genuine need for
// `metallurgy/alloy-selection` should not have to decide whether that is
// "data" or "other".
//
// So: no seed list, no synonyms, no inference. The namespace is the
// whole rule, the full tag stays the market's identity, and a sector
// appears the first time a task carries one.
//
// Presentation-layer only. On the wire a capability is a free-form string
// on a task -- the hub and the chain have no notion of a sector -- and
// nothing here changes what the protocol stores or validates.
//
// Nothing in `src/lib/` may import React.

/** Where a tag with no namespace trades.
 *
 * Tasks posted before namespaced tags existed carry bare strings like
 * `python`, and a real hub accepts any string as a tag forever. Those
 * stay visible and grouped rather than vanishing or inventing a sector
 * nobody named. */
export const OTHER_SECTOR = "other";

/** What separates a sector from a market inside one tag. Only the *first*
 * one counts: `software/python/asyncio` is the `software` sector's
 * `python/asyncio` market, so an agent may nest further without the board
 * having to know what the deeper levels mean. */
const SECTOR_SEPARATOR = "/";

/** The sector a capability trades in: everything before the first `/`,
 * or `other` for a tag that names none.
 *
 * No lookup, no normalisation, no correction. `Python` and `python` are
 * different tags because the hub treats them as different tags, and a
 * board that merged them here would group markets that the task list
 * then refuses to group -- the hub's capability filter is an exact
 * comparison. */
export function sectorOf(capability: string): string {
  const cut = capability.indexOf(SECTOR_SEPARATOR);
  if (cut > 0) {
    const sector = capability.slice(0, cut).trim();
    // A tag that is only a separator, or starts with one, has named no
    // sector -- `other` rather than an empty heading.
    if (sector) return sector;
  }
  return OTHER_SECTOR;
}

/** What to call a market on screen: the tag minus its sector, since the
 * sector is already the panel's heading. The *full* tag stays the
 * identity -- it is what links to the task list and what the hub filters
 * on -- so this is only ever a label. */
export function marketLabel(capability: string): string {
  const cut = capability.indexOf(SECTOR_SEPARATOR);
  if (cut > 0 && cut < capability.length - 1) {
    const label = capability.slice(cut + 1).trim();
    if (label) return label;
  }
  return capability;
}
