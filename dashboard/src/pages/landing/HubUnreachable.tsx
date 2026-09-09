import { hubUrl } from "../../lib/hub";

/** Says, above the fold, that this page never reached its hub.
 *
 * The board is honest about being empty -- "nothing on the tape yet",
 * "no work posted yet", an empty carousel -- and that is exactly the
 * problem it had: a site whose hub is unreachable rendered as a *quiet*
 * board, indistinguishable from a launch day nobody had posted to. The
 * one line that said otherwise belonged to the activity panel, which is
 * two screens down.
 *
 * It is the likeliest way a deployment goes wrong, too. The shipped
 * `index.html` carries a placeholder hub, deliberately treated as unset
 * (§5.1), so an operator who forgets that one edit gets a page that
 * loads perfectly and can never load anything else. `curl` cannot see
 * it: the document is served correctly, and only the requests the
 * browser makes afterwards fail.
 *
 * So the address is printed. It is not a secret -- it is in the page
 * source, one view-source away -- and it is the whole diagnosis: an
 * operator seeing `127.0.0.1:9100` on a public site has just been told
 * the meta tag is still the placeholder, and a visitor seeing the real
 * hostname has been told the API is down rather than the market quiet.
 */
export default function HubUnreachable() {
  return (
    <div className="itx-hub-down" role="status">
      <span className="itx-hub-down-dot" aria-hidden="true" />
      <p>
        this site can&apos;t reach its hub, so nothing below is live.{" "}
        <span className="itx-hub-down-where">
          tried <code>{hubUrl()}</code>
        </span>
      </p>
    </div>
  );
}
