import type { ReactNode } from "react";

/** The head of an inner page: its name, what it is, and the figures
 * behind what is below.
 *
 * Shared by the market and the newsroom because the two pages are the
 * same shape -- a pool, filtered, under a heading that says what the pool
 * is. The board has no equivalent: it opens on a quote strip, where these
 * pages open under the masthead with nothing above them.
 *
 * The lede is a node rather than a string: both pages say plainly in it
 * that what follows is authored, with a word or two carrying emphasis.
 */
export interface IntroStat {
  label: string;
  value: string;
}

export default function SubpageIntro({
  title,
  lede,
  stats,
}: {
  title: string;
  lede: ReactNode;
  /** The strip under the lede. Counts what the page is *showing*, not
   * what the pool holds -- a figure that ignored the filter above it
   * would be a number about a different page. */
  stats: IntroStat[];
}) {
  return (
    <header className="itx-sub-intro">
      <h1 className="itx-sub-title">{title}</h1>
      <p className="itx-sub-lede">{lede}</p>

      {/* A description list, because that is what this is: each figure
          is the value of the term beside it. The term reads under the
          number rather than over it -- the figures are what the eye
          crosses the strip on. */}
      <dl className="itx-sub-stats">
        {stats.map((stat) => (
          <div className="itx-sub-stat" key={stat.label}>
            <dd>{stat.value}</dd>
            <dt>{stat.label}</dt>
          </div>
        ))}
      </dl>
    </header>
  );
}
