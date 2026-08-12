import { useEffect, useId, useRef, useState, type KeyboardEvent } from "react";

interface Props {
  /** The committed filter value, i.e. what the URL says. */
  value: string;
  /** Everything that could be picked, already in display order. */
  options: string[];
  placeholder: string;
  /** Accessible name for the text field. */
  label: string;
  onChange: (value: string) => void;
  /** How an option prints. The value committed is always the option
   * itself -- this only changes what the row says, which is how the
   * market picker shows `python` for `coding/python`. */
  renderOption?: (option: string) => string;
}

/** A filter field that is both a search box and a dropdown.
 *
 * A plain text input was the wrong control for the capability filter: it
 * only helps someone who already knows a tag exists, and the tags are
 * free-form strings a poster invents, so nobody arriving at the board
 * knows one to type. A plain `<select>` is the wrong control too -- a
 * real hub has hundreds of markets, which is a scroll, not a menu.
 *
 * So: type to narrow, or open the list and browse. A native `<datalist>`
 * would have been a quarter of this code and does the typing half well,
 * but its dropdown has no affordance a reader can see -- there is no
 * caret to click, and no way to browse without first typing a character
 * that eliminates most of the list. The caret is the whole point here.
 *
 * Free text still commits on Enter, unchanged from the input this
 * replaces: the option list is a superset of what the board currently
 * holds, not a whitelist, and a tag the list has never heard of is a
 * perfectly good filter for a hub that has just started using it.
 */
export default function ComboFilter({
  value,
  options,
  placeholder,
  label,
  onChange,
  renderOption,
}: Props) {
  const [draft, setDraft] = useState(value);
  const [open, setOpen] = useState(false);
  const [highlight, setHighlight] = useState(-1);
  const wrapper = useRef<HTMLDivElement>(null);
  const listId = useId();

  // The URL is the source of truth: a filter cleared by the "clear"
  // button or by pressing Back has to empty the field too, or the box
  // goes on showing a filter that is no longer applied.
  useEffect(() => {
    setDraft(value);
  }, [value]);

  // Typing narrows the list; an untouched field (draft still equal to
  // the committed value) shows everything, so clicking the caret on an
  // already-filtered field browses rather than showing one row.
  const needle = draft.trim().toLowerCase();
  const matches =
    needle === "" || draft === value
      ? options
      : options.filter((option) => option.toLowerCase().includes(needle));

  function commit(next: string) {
    setOpen(false);
    setHighlight(-1);
    setDraft(next);
    onChange(next);
  }

  function onKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      if (!open) {
        setOpen(true);
        return;
      }
      const step = event.key === "ArrowDown" ? 1 : -1;
      const count = matches.length;
      if (count === 0) return;
      setHighlight((current) => (current + step + count) % count);
      return;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      commit(open && highlight >= 0 ? matches[highlight] : draft.trim());
      return;
    }
    if (event.key === "Escape") {
      setOpen(false);
      setHighlight(-1);
    }
  }

  return (
    <div
      className="itx-combo"
      ref={wrapper}
      // Closing on blur rather than on a document click keeps the
      // listener off `document`; `relatedTarget` is what tells a click
      // on one of our own options apart from a click that leaves.
      onBlur={(event) => {
        if (!wrapper.current?.contains(event.relatedTarget as Node | null)) {
          setOpen(false);
          setHighlight(-1);
        }
      }}
    >
      <input
        className="itx-input itx-combo-input"
        value={draft}
        placeholder={placeholder}
        aria-label={label}
        role="combobox"
        aria-expanded={open}
        aria-controls={listId}
        aria-autocomplete="list"
        onChange={(event) => {
          setDraft(event.target.value);
          setOpen(true);
          setHighlight(-1);
        }}
        onKeyDown={onKeyDown}
      />
      <button
        type="button"
        className="itx-combo-toggle"
        aria-label={open ? `Hide ${label} options` : `Show ${label} options`}
        aria-expanded={open}
        onClick={() => {
          setOpen((was) => !was);
          setHighlight(-1);
        }}
      >
        ▾
      </button>
      {open && (
        <ul className="itx-combo-list" id={listId} role="listbox" aria-label={label}>
          {matches.length === 0 && (
            <li className="itx-combo-empty" role="presentation">
              No match
            </li>
          )}
          {value !== "" && (
            <li role="presentation">
              <button
                type="button"
                className="itx-combo-option itx-combo-clear"
                onClick={() => commit("")}
              >
                Any
              </button>
            </li>
          )}
          {matches.map((option, index) => (
            <li key={option} role="presentation">
              <button
                type="button"
                role="option"
                aria-selected={option === value}
                // `renderOption` can collapse two distinct tags to the
                // same label -- `coding/rust` and a bare `rust` both
                // print as "rust" -- so the row carries the tag it will
                // actually commit.
                title={option}
                className={
                  "itx-combo-option" +
                  (index === highlight ? " highlight" : "") +
                  (option === value ? " selected" : "")
                }
                // `onMouseDown` would fire before blur and save the
                // `relatedTarget` dance, but it also breaks keyboard
                // activation; the blur guard above already handles it.
                onClick={() => commit(option)}
              >
                {renderOption ? renderOption(option) : option}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
