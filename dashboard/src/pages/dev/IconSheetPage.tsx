import type { ReactNode } from "react";
import ProfileIcon from "../../components/ProfileIcon";
import {
  ICON_ACCESSORIES,
  ICON_ANIMALS,
  ICON_EYES,
  ICON_MOUTHS,
  iconSpec,
} from "../../lib/profileIcon";
import type { IconSpec } from "../../lib/profileIcon";

/** Dev-only contact sheet for tuning `ProfileIcon`'s `ALIGN` table.
 *
 * Routed only when `import.meta.env.DEV` -- see App.tsx.
 *
 * The tuning question is always "does this one piece sit right on all
 * six animals", so every view is one row per piece with the six animals
 * side by side -- a piece that drifts shows up as a stepped line rather
 * than something you have to hold in your head between two screens.
 * `?piece=crown&size=240` isolates one row big.
 *
 * Icons are found by brute-force search for a pubkey whose spec matches
 * the wanted cell. That exercises the real `iconSpec` rather than a
 * bypass that could drift from it, and at dev-page scale it is free. */
function findPubkey(match: (spec: IconSpec) => boolean): string | null {
  for (let i = 0; i < 400_000; i++) {
    const candidate = `probe-${i}`;
    if (match(iconSpec(candidate))) return candidate;
  }
  return null;
}

/** All six animals wearing one specific thing. */
function Row({
  label,
  size,
  match,
}: {
  label: string;
  size: number;
  match: (spec: IconSpec) => boolean;
}) {
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 6 }}>
      <code style={{ width: 104, fontSize: 11 }}>{label}</code>
      {ICON_ANIMALS.map((animal) => {
        const pk = findPubkey((s) => s.animal === animal && match(s));
        return (
          <div key={animal} title={`${animal} · ${label}`} style={{ textAlign: "center" }}>
            {pk ? <ProfileIcon pubkey={pk} size={size} /> : <span style={{ fontSize: 11 }}>—</span>}
            <div style={{ fontFamily: "monospace", fontSize: 10 }}>{animal}</div>
          </div>
        );
      })}
    </div>
  );
}

/** Bare faces: animal plus one eye piece, nothing worn. This is the
 * view for tuning `ALIGN`, and it has to come first -- an accessory
 * placement judged against a misaligned animal just bakes the
 * misalignment into the accessory. */
function AlignRow({ size, eyes }: { size: number; eyes: IconSpec["eyes"] | null }) {
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 6 }}>
      <code style={{ width: 104, fontSize: 11 }}>{eyes ?? "no eyes"}</code>
      {ICON_ANIMALS.map((animal) => (
        <div key={animal} style={{ textAlign: "center" }}>
          <ProfileIcon
            pubkey=""
            size={size}
            spec={{
              animal,
              eyes,
              mouth: animal === "pig" ? null : "neutral",
              accessory: null,
              body: "#91c4f2",
              background: "#e9f2ed",
            }}
          />
          <div style={{ fontFamily: "monospace", fontSize: 10 }}>{animal}</div>
        </div>
      ))}
    </div>
  );
}

export default function IconSheetPage() {
  const params = new URLSearchParams(window.location.search);
  const piece = params.get("piece");
  const size = Number(params.get("size") ?? 0) || (piece ? 190 : 104);

  // The eight reference images, reproduced: same animal, same pieces,
  // same order. This is the view that answers "does it match the art
  // direction" -- everything else only answers "is it consistent".
  if (piece === "refs") {
    const animal = (params.get("animal") ?? "cat") as IconSpec["animal"];
    const REFS: [IconSpec["eyes"], IconSpec["mouth"], IconSpec["accessory"]][] = [
      ["wink", "neutral", "partyhat"],
      ["dead", "cutesy", "sunglasses"],
      ["open", "neutral", "glasses"],
      ["dead", "neutral", "crown"],
      ["open", "neutral", "headphones"],
      ["wink", "mustache", "bow"],
      ["wink", "neutral", "tie"],
      ["dead", "neutral", "eyepatch"],
    ];
    return (
      <Sheet>
        <div style={{ display: "flex", flexWrap: "wrap", gap: 8 }}>
          {REFS.map(([eyes, mouth, accessory]) => (
            <div key={accessory} style={{ textAlign: "center" }}>
              <ProfileIcon
                pubkey=""
                size={size}
                spec={{
                  animal,
                  eyes,
                  mouth: animal === "pig" ? null : mouth,
                  accessory,
                  body: "#82c6f7",
                  background: "#ffffff",
                }}
              />
              <div style={{ fontFamily: "monospace", fontSize: 10 }}>{accessory}</div>
            </div>
          ))}
        </div>
      </Sheet>
    );
  }

  if (piece === "align") {
    return (
      <Sheet>
        <AlignRow size={size} eyes="open" />
        <AlignRow size={size} eyes="dead" />
        <AlignRow size={size} eyes={null} />
      </Sheet>
    );
  }

  if (piece) {
    const isEyes = (ICON_EYES as readonly string[]).includes(piece);
    const isMouth = (ICON_MOUTHS as readonly string[]).includes(piece);
    return (
      <Sheet>
        <Row
          label={piece}
          size={size}
          match={(s) =>
            isEyes ? s.eyes === piece : isMouth ? s.mouth === piece : s.accessory === piece
          }
        />
      </Sheet>
    );
  }

  return (
    <Sheet>
      <h2 style={h2}>accessories — each must sit identically on all six</h2>
      {ICON_ACCESSORIES.map((accessory) => (
        <Row key={accessory} label={accessory} size={size} match={(s) => s.accessory === accessory} />
      ))}

      <h2 style={h2}>eyes</h2>
      {ICON_EYES.map((eyes) => (
        <Row key={eyes} label={eyes} size={size} match={(s) => s.eyes === eyes} />
      ))}

      <h2 style={h2}>mouths (the pig has none — its snout is body artwork)</h2>
      {ICON_MOUTHS.map((mouth) => (
        <Row key={mouth} label={mouth} size={size} match={(s) => s.mouth === mouth} />
      ))}

      <h2 style={h2}>as real traffic</h2>
      <div style={{ display: "flex", flexWrap: "wrap", gap: 4, maxWidth: 1000 }}>
        {Array.from({ length: 40 }, (_, i) => (
          <ProfileIcon key={i} pubkey={`02${i.toString(16).padStart(64, "a")}`} size={30} />
        ))}
      </div>
    </Sheet>
  );
}

const h2 = { fontFamily: "monospace", fontSize: 12, marginTop: 20 } as const;

function Sheet({ children }: { children: ReactNode }) {
  return (
    <div style={{ padding: 20, background: "#e9f2ed", minHeight: "100vh" }}>
      <h1 style={{ fontFamily: "monospace", fontSize: 14 }}>
        profile icons — ?piece=crown&amp;size=240 isolates one row
      </h1>
      {children}
    </div>
  );
}
