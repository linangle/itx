import { formatCount } from "../lib/format";

interface Props {
  page: number;
  pageSize: number;
  total: number;
  onPageChange: (page: number) => void;
}

/** Prev/next pager with an explicit "showing X–Y of Z" range.
 *
 * Paging happens **client-side**, over the already-fetched and filtered
 * set, rather than through the hub's `?offset=&limit=`. That's not
 * laziness: kind filtering has no server-side equivalent (the hub has no
 * `?kind=`), so a server-side page of 50 would arrive, get filtered down
 * to whatever matched, and render a page of unpredictable size --
 * sometimes empty, while later pages still held matches. Filtering first
 * and paging second is the only ordering that yields correct page sizes.
 *
 * The hub's `X-Total-Count` still does real work upstream: `listAllTasks`
 * uses it to know when it has walked the whole board.
 *
 * Renders nothing for a single page -- a pager that can't page is noise.
 */
export default function Pager({ page, pageSize, total, onPageChange }: Props) {
  const pageCount = Math.max(1, Math.ceil(total / pageSize));
  if (pageCount <= 1) return null;

  const first = page * pageSize + 1;
  const last = Math.min(total, (page + 1) * pageSize);

  return (
    <div className="itx-pager">
      <span className="num">
        {formatCount(first)}–{formatCount(last)} of {formatCount(total)}
      </span>
      <span className="itx-pager-spacer" />
      <button type="button" onClick={() => onPageChange(page - 1)} disabled={page === 0}>
        ← Prev
      </button>
      <span className="num">
        {page + 1} / {pageCount}
      </span>
      <button
        type="button"
        onClick={() => onPageChange(page + 1)}
        disabled={page >= pageCount - 1}
      >
        Next →
      </button>
    </div>
  );
}
