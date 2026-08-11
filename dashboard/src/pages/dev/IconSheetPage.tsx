import ProfileIcon from "../../components/ProfileIcon";
import {
  ICON_ACCESSORIES,
  ICON_ANIMALS,
  ICON_EYES,
  ICON_MOUTHS,
  iconSpec,
} from "../../lib/profileIcon";

/** Dev-only contact sheet for tuning `ProfileIcon`'s anchor tables.
 *
 * Routed only when `import.meta.env.DEV` -- see App.tsx. Two views:
 * a systematic matrix (every animal against every accessory, then
 * every eye and mouth) for checking placement, and a row of
 * hash-derived icons for checking what real traffic will look like.
 *
 * Brute-force search for a pubkey that hits a wanted (animal,
 * accessory) cell: fine at dev-page scale, and it exercises the real
 * `iconSpec` path rather than a bypass that could drift from it. */
function findPubkey(match: (spec: ReturnType<typeof iconSpec>) => boolean): string | null {
  for (let i = 0; i < 200_000; i++) {
    const candidate = `probe-${i}`;
    if (match(iconSpec(candidate))) return candidate;
  }
  return null;
}

const SIZE = 150;

export default function IconSheetPage() {
  // ?only=pig,rabbit filters the matrix rows -- the pane's screenshot
  // capture is unreliable when scrolled, so tuning works by filtering
  // the row of interest to the top instead.
  const only = new URLSearchParams(window.location.search).get("only")?.split(",");
  const animals = ICON_ANIMALS.filter((a) => !only || only.includes(a));
  return (
    <div style={{ padding: 24, background: "#e9f2ed", minHeight: "100vh" }}>
      <h1 style={{ fontFamily: "monospace", fontSize: 16 }}>profile icon contact sheet</h1>

      <h2 style={{ fontFamily: "monospace", fontSize: 13 }}>animal x accessory</h2>
      <table style={{ borderCollapse: "collapse" }}>
        <tbody>
          {animals.map((animal) => (
            <tr key={animal}>
              <td style={{ fontFamily: "monospace", fontSize: 11, paddingRight: 8 }}>{animal}</td>
              {ICON_ACCESSORIES.map((accessory) => {
                const pk = findPubkey((s) => s.animal === animal && s.accessory === accessory);
                return (
                  <td key={accessory} style={{ padding: 2 }} title={`${animal}/${accessory}`}>
                    {pk ? <ProfileIcon pubkey={pk} size={SIZE} /> : "?"}
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
      </table>

      <h2 style={{ fontFamily: "monospace", fontSize: 13 }}>animal x eyes / mouths</h2>
      <table style={{ borderCollapse: "collapse" }}>
        <tbody>
          {animals.map((animal) => (
            <tr key={animal}>
              <td style={{ fontFamily: "monospace", fontSize: 11, paddingRight: 8 }}>{animal}</td>
              {ICON_EYES.map((eyes) => {
                const pk = findPubkey((s) => s.animal === animal && s.eyes === eyes);
                return (
                  <td key={eyes} style={{ padding: 2 }} title={`${animal}/${eyes}`}>
                    {pk ? <ProfileIcon pubkey={pk} size={SIZE} /> : "?"}
                  </td>
                );
              })}
              {ICON_MOUTHS.map((mouth) => {
                const pk = findPubkey((s) => s.animal === animal && s.mouth === mouth);
                return (
                  <td key={mouth} style={{ padding: 2 }} title={`${animal}/${mouth}`}>
                    {pk ? <ProfileIcon pubkey={pk} size={SIZE} /> : "?"}
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
      </table>

      <h2 style={{ fontFamily: "monospace", fontSize: 13 }}>as real traffic (hash-derived, small)</h2>
      <div style={{ display: "flex", flexWrap: "wrap", gap: 4, maxWidth: 900 }}>
        {Array.from({ length: 48 }, (_, i) => (
          <ProfileIcon key={i} pubkey={`02${i.toString(16).padStart(64, "a")}`} size={28} />
        ))}
      </div>
    </div>
  );
}
