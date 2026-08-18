/** Desks: the one vocabulary the prediction market and the newsroom
 * share, and the counting behind the filter both pages carry.
 *
 * A "desk" is a story's or a market's `category` — weather, spaceflight,
 * energy, and so on. The two sample pools use the same words on purpose:
 * the newsroom is what the agents read and the market is what they
 * priced off it, so a reader who filters one to `energy` and then walks
 * to the other should land on the same subject rather than on a
 * different taxonomy.
 *
 * Generic over the two sample types rather than written twice: both
 * carry a `category`, and that is all this needs to know about them.
 */

/** Anything filed under a desk. */
export interface Filed {
  category: string;
}

/** What the filter shows for the desk it is currently on. `ALL_DESKS`
 * is not a category any item carries — it is the filter's "off". */
export const ALL_DESKS = "all";

export interface Desk {
  name: string;
  count: number;
}

/** Every desk in a pool, busiest first, with the count each holds.
 *
 * Derived rather than authored: a hard-coded desk list would keep
 * offering a desk after its last story aged out, and would miss a new
 * one the moment the pool grew — which for a pool that is meant to be
 * swapped for a feed is the failure that matters.
 *
 * Ties break alphabetically so the row is stable between renders rather
 * than depending on the pool's own order. */
export function desksOf(items: Filed[]): Desk[] {
  const counts = new Map<string, number>();
  for (const item of items) {
    counts.set(item.category, (counts.get(item.category) ?? 0) + 1);
  }
  return [...counts]
    .map(([name, count]) => ({ name, count }))
    .sort((a, b) => b.count - a.count || a.name.localeCompare(b.name));
}

/** The pool cut to one desk, or all of it. Order is preserved — the
 * caller has already decided what order means. */
export function onDesk<T extends Filed>(items: T[], desk: string): T[] {
  if (desk === ALL_DESKS) return items;
  return items.filter((item) => item.category === desk);
}
