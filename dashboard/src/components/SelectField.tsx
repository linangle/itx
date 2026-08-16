import { useEffect, useId, useRef, useState, type KeyboardEvent } from "react";
import Triangle from "./Triangle";

export interface SelectOption {
  value: string;
  label: string;
}

interface Props {
  value: string;
  onChange: (value: string) => void;
  /** Accessible name. The control has no visible label -- the selected
   * option is the label -- so this is the only thing naming it. */
  label: string;
  options: SelectOption[];
}

/** A one-of-N picker that draws its own menu.
 *
 * This was a native `<select>` with the site's caret painted over it,
 * which fixed the closed state and left the open one alone -- and the
 * open one is the half the platform draws. Beside it in the same filter
 * bar sits `ComboFilter`, whose menu is ours: a 14px panel, a hairline
 * border, 12px rows. Clicking one filter got a macOS popover with a
 * checkmark and system rounding; clicking the next got the site's list.
 * Two dropdowns, two design languages, one row of controls.
 *
 * There is no styling fix for that -- an open `<select>`'s menu is not
 * in the page, and `option` takes almost no CSS in any browser. So the
 * menu is a listbox of buttons here, sharing `.itx-menu` with the combo
 * box so the two cannot drift: same panel, same rows, same highlight.
 * The closed control keeps `.itx-select`, so the field itself looks
 * exactly as it did.
 *
 * What that costs is the keyboard and a11y behaviour a `<select>` had
 * for free, re-implemented below: arrows move, Enter and Space commit,
 * Escape closes, opening starts on the current value, and the roles say
 * combobox/listbox/option. What it buys is one dropdown on the site.
 */
export default function SelectField({ value, onChange, label, options }: Props) {
  const [open, setOpen] = useState(false);
  const [highlight, setHighlight] = useState(-1);
  const wrapper = useRef<HTMLDivElement>(null);
  const listId = useId();

  const selected = options.findIndex((option) => option.value === value);
  const current = selected >= 0 ? options[selected] : undefined;

  // A filter cleared elsewhere -- the "clear filters" button, the Back
  // button -- moves the highlight with it, so reopening the menu starts
  // on what is actually selected rather than on the last thing hovered.
  useEffect(() => {
    setHighlight(-1);
  }, [value]);

  function commit(next: string) {
    setOpen(false);
    setHighlight(-1);
    onChange(next);
  }

  function onKeyDown(event: KeyboardEvent<HTMLButtonElement>) {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const step = event.key === "ArrowDown" ? 1 : -1;
      if (!open) {
        setOpen(true);
        // Opening with an arrow lands on the current value, not on the
        // end of the list -- the first press should be one step from
        // where you are.
        setHighlight(selected >= 0 ? selected : 0);
        return;
      }
      const count = options.length;
      if (count === 0) return;
      const from = highlight >= 0 ? highlight : selected;
      setHighlight((from + step + count) % count);
      return;
    }
    if (event.key === "Enter" || event.key === " ") {
      // A `<button>` turns both keys into a click, which would toggle
      // the menu shut instead of choosing the highlighted row.
      if (open && highlight >= 0) {
        event.preventDefault();
        commit(options[highlight].value);
      }
      return;
    }
    if (event.key === "Escape" && open) {
      event.preventDefault();
      setOpen(false);
      setHighlight(-1);
    }
  }

  return (
    <div
      className="itx-select-wrap"
      ref={wrapper}
      // Same guard the combo box uses: `relatedTarget` tells a click on
      // one of our own options apart from a click that leaves.
      onBlur={(event) => {
        if (!wrapper.current?.contains(event.relatedTarget as Node | null)) {
          setOpen(false);
          setHighlight(-1);
        }
      }}
    >
      <button
        type="button"
        className="itx-select"
        aria-label={label}
        role="combobox"
        aria-expanded={open}
        aria-controls={listId}
        onKeyDown={onKeyDown}
        onClick={() => {
          setOpen((was) => !was);
          setHighlight(-1);
        }}
      >
        {current ? current.label : ""}
      </button>
      <span className="itx-select-caret" aria-hidden="true">
        <Triangle direction="down" />
      </span>
      {open && (
        <ul className="itx-menu" id={listId} role="listbox" aria-label={label}>
          {options.map((option, index) => (
            <li key={option.value || "any"} role="presentation">
              <button
                type="button"
                role="option"
                aria-selected={option.value === value}
                className={
                  "itx-menu-option" +
                  (index === highlight ? " highlight" : "") +
                  (option.value === value ? " selected" : "")
                }
                onClick={() => commit(option.value)}
              >
                {option.label}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
