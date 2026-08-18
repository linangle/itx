/** One row of choices, as pills: the control both inner pages filter
 * and order with.
 *
 * Generic over what it is choosing rather than written once per page:
 * the market page picks a desk and an order, the newsroom picks a desk
 * and an order, and four hand-written pill rows would be four places to
 * fix the same hover state.
 *
 * It follows the board's own pill grammar -- 999px, muted at rest, the
 * ground's colour under the pointer, a lifted fill on the one you are on
 * (see `.itx-board-navlist`) -- with a hairline added, because a
 * horizontal row of borderless words reads as a sentence rather than as
 * a set of buttons.
 *
 * Buttons, not links: these filter what is already on the page. Nothing
 * is fetched and nowhere is navigated to, so a URL for each combination
 * would be a promise the page does not keep. `aria-pressed` is what
 * carries the state to a screen reader, which is the accurate reading of
 * a toggle that changes the view in place.
 */
export interface PillOption<T extends string> {
  value: T;
  label: string;
  /** How many items sit behind this choice, when the choice is a
   * filter. Ordering pills have no count -- "17" beside "newest" would
   * be a number about nothing. */
  count?: number;
}

export default function FilterPills<T extends string>({
  label,
  options,
  value,
  onChange,
}: {
  /** What the row chooses, said in a word: "desk", "order". Rendered
   * beside the pills, and used as the group's accessible name. */
  label: string;
  options: PillOption<T>[];
  value: T;
  onChange: (value: T) => void;
}) {
  return (
    <div className="itx-filter" role="group" aria-label={label}>
      <span className="itx-filter-label">{label}</span>
      <div className="itx-filter-pills">
        {options.map((option) => {
          const on = option.value === value;
          return (
            <button
              key={option.value}
              type="button"
              className={on ? "itx-filter-pill is-on" : "itx-filter-pill"}
              aria-pressed={on}
              onClick={() => onChange(option.value)}
            >
              {option.label}
              {option.count !== undefined && (
                <span className="itx-filter-count">{option.count}</span>
              )}
            </button>
          );
        })}
      </div>
    </div>
  );
}
