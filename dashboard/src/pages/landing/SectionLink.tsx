import { Link } from "react-router-dom";

/** A board section's door: its name and an arrow, as one link.
 *
 * An arrow on its own is a control whose target you have to infer, and
 * the words next to it were the part that said where it went -- so the
 * words are inside the link and the whole thing is the hit area.
 *
 * It still *reads* as the section's heading: the name keeps the label's
 * weight and the page's ink, and only the arrow carries the link colour.
 * Hovering lifts both and nudges the arrow the way it points. */
export default function SectionLink({
  to,
  label,
  describedAs,
}: {
  to: string;
  /** The section's name, which is also the link's text. */
  label: string;
  /** What the link goes to, for a screen reader — "prediction market"
   * alone would be read as a heading rather than as a destination. */
  describedAs: string;
}) {
  return (
    <Link className="itx-section-link" to={to} aria-label={describedAs} title={describedAs}>
      <span className="itx-board-label">{label}</span>
      <svg
        className="itx-section-arrow"
        viewBox="0 0 16 16"
        width="16"
        height="16"
        aria-hidden="true"
      >
        <path
          d="M2 8h11M9 3.5 13.5 8 9 12.5"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.8"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      </svg>
    </Link>
  );
}
