import SearchIcon from "./SearchIcon";

interface Props {
  value: string;
  onChange: (value: string) => void;
  placeholder: string;
  /** Accessible name. Says what is searched and by what, since the
   * placeholder has room for neither. */
  label: string;
}

/** A search box shaped like the rest of the filter bar.
 *
 * The same pill as `.itx-input` and `ComboFilter`, with the magnifier
 * inside it and a clear button that appears once there is something to
 * clear. `type="search"` for the semantics and the browser's own Escape
 * handling; the native clear affordance is suppressed in CSS because it
 * differs per browser and would sit beside ours.
 *
 * Uncontrolled debouncing is deliberately *not* here: the field reports
 * every keystroke, and whoever owns the query decides whether to wait
 * before acting on it (`useDebounced`). A field that lies about its own
 * value to save a request is a field you cannot type in.
 */
export default function SearchField({ value, onChange, placeholder, label }: Props) {
  return (
    <div className="itx-search">
      <SearchIcon />
      <input
        type="search"
        className="itx-search-input"
        value={value}
        placeholder={placeholder}
        aria-label={label}
        onChange={(event) => onChange(event.target.value)}
      />
      {value !== "" && (
        <button
          type="button"
          className="itx-search-clear"
          aria-label="Clear search"
          onClick={() => onChange("")}
        >
          ×
        </button>
      )}
    </div>
  );
}
