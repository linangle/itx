import type { ReactNode } from "react";
import Triangle from "./Triangle";

interface Props {
  value: string;
  onChange: (value: string) => void;
  /** Accessible name. The control has no visible label -- the selected
   * option is the label -- so this is the only thing naming it. */
  label: string;
  children: ReactNode;
}

/** A `<select>` wearing the site's own caret.
 *
 * The wrapper exists for one reason: a `<select>` cannot hold a child
 * element, so a caret drawn inside it has to be a background image, and
 * a background image cannot see `currentColor`. Every other arrow on
 * the site is a `<Triangle>`; putting this one on a wrapping span makes
 * it one too, instead of a `data:` URI holding a second copy of the
 * same path that has to be kept in step by hand.
 */
export default function SelectField({ value, onChange, label, children }: Props) {
  return (
    <span className="itx-select-wrap">
      <select
        className="itx-select"
        value={value}
        aria-label={label}
        onChange={(event) => onChange(event.target.value)}
      >
        {children}
      </select>
      <span className="itx-select-caret" aria-hidden="true">
        <Triangle direction="down" />
      </span>
    </span>
  );
}
