import { afterEach, describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";
import { useSwipe } from "./useSwipe";
import type { SwipeDirection } from "./useSwipe";

/** A drag is a stream of pointer events, so the tests speak in them
 * rather than in `fireEvent` one line at a time. Defaults to a touch
 * because that is the case the hook exists for. */
function drag(
  el: HTMLElement,
  moves: Array<[number, number]>,
  { pointerType = "touch", release = true }: { pointerType?: string; release?: boolean } = {},
) {
  const at = (type: string, [clientX, clientY]: [number, number]) =>
    el.dispatchEvent(
      new PointerEvent(type, {
        pointerId: 1,
        pointerType,
        isPrimary: true,
        clientX,
        clientY,
        bubbles: true,
      }),
    );

  act(() => {
    at("pointerdown", moves[0]);
    for (const point of moves.slice(1)) at("pointermove", point);
    if (release) at("pointerup", moves[moves.length - 1]);
  });
}

/** A trackpad's version of the same gesture: a burst of small deltas
 * rather than one move. Returns the events so a test can ask whether the
 * browser's own handling was taken away. */
function scroll(el: HTMLElement, deltas: Array<[number, number]>) {
  const events = deltas.map(
    ([deltaX, deltaY]) => new WheelEvent("wheel", { deltaX, deltaY, bubbles: true, cancelable: true }),
  );
  act(() => {
    for (const e of events) el.dispatchEvent(e);
  });
  return events;
}

function Harness({ onSwipe }: { onSwipe: (direction: SwipeDirection) => void }) {
  const [ref, offset, dragging] = useSwipe<HTMLDivElement>(onSwipe);
  return (
    <div ref={ref} data-testid="row" data-dragging={dragging || undefined}>
      {offset}
    </div>
  );
}

describe("useSwipe", () => {
  // The trackpad path decides a gesture is over by a quiet timer, so
  // the clock is the tests' to move. Faked for the whole suite rather
  // than per test: real timers here would mean sleeping through the
  // idle window in the middle of an assertion.
  vi.useFakeTimers();
  afterEach(() => {
    vi.clearAllTimers();
  });

  it("asks for the next item when the drag goes left, and the previous when it goes right", () => {
    const onSwipe = vi.fn();
    render(<Harness onSwipe={onSwipe} />);
    const row = screen.getByTestId("row");

    drag(row, [
      [200, 50],
      [140, 52],
      [90, 54],
    ]);
    expect(onSwipe).toHaveBeenCalledWith(1);

    drag(row, [
      [90, 50],
      [150, 48],
      [200, 46],
    ]);
    expect(onSwipe).toHaveBeenLastCalledWith(-1);
  });

  it("ignores a drag that stops short of the commit distance", () => {
    const onSwipe = vi.fn();
    render(<Harness onSwipe={onSwipe} />);

    drag(screen.getByTestId("row"), [
      [200, 50],
      [176, 50],
    ]);

    expect(onSwipe).not.toHaveBeenCalled();
  });

  it("leaves vertical drags to the page", () => {
    const onSwipe = vi.fn();
    render(<Harness onSwipe={onSwipe} />);
    const row = screen.getByTestId("row");

    // Down the screen and well past the commit distance sideways: a
    // scroll that drifts is still a scroll, because the axis is settled
    // by the first few pixels and never revisited.
    drag(row, [
      [200, 50],
      [196, 90],
      [100, 300],
    ]);

    expect(onSwipe).not.toHaveBeenCalled();
    expect(row).not.toHaveAttribute("data-dragging");
  });

  it("ignores the mouse, which is selecting text or about to click a link", () => {
    const onSwipe = vi.fn();
    render(<Harness onSwipe={onSwipe} />);

    drag(
      screen.getByTestId("row"),
      [
        [200, 50],
        [140, 50],
        [90, 50],
      ],
      { pointerType: "mouse" },
    );

    expect(onSwipe).not.toHaveBeenCalled();
  });

  it("follows the finger while dragging, damped, and lets go on release", () => {
    render(<Harness onSwipe={() => {}} />);
    const row = screen.getByTestId("row");

    drag(
      row,
      [
        [200, 50],
        [100, 50],
      ],
      { release: false },
    );
    // 100px of travel at the damping factor, and the row says it is
    // being dragged so the spring-back transition stays off.
    expect(row).toHaveTextContent("-35");
    expect(row).toHaveAttribute("data-dragging");

    // Past the pull ceiling the row stops following, or a long drag
    // would haul the panels clear of their own column.
    drag(
      row,
      [
        [200, 50],
        [-400, 50],
      ],
      { release: false },
    );
    expect(row).toHaveTextContent("-56");

    act(() => {
      row.dispatchEvent(
        new PointerEvent("pointerup", {
          pointerId: 1,
          pointerType: "touch",
          isPrimary: true,
          clientX: -400,
          clientY: 50,
          bubbles: true,
        }),
      );
    });
    expect(row).toHaveTextContent("0");
    expect(row).not.toHaveAttribute("data-dragging");
  });

  it("pages on a trackpad's horizontal scroll, in the direction of the scroll", () => {
    const onSwipe = vi.fn();
    render(<Harness onSwipe={onSwipe} />);
    const row = screen.getByTestId("row");

    // Scrolling right asks for the market to the right.
    scroll(row, [
      [12, 1],
      [20, 0],
      [24, -1],
      [20, 0],
    ]);
    expect(onSwipe).toHaveBeenCalledWith(1);
    // Back at rest as the new market lands, not held off centre.
    expect(row).toHaveTextContent("0");
    expect(row).not.toHaveAttribute("data-dragging");

    act(() => vi.advanceTimersByTime(300));
    scroll(row, [
      [-30, 0],
      [-40, 2],
    ]);
    expect(onSwipe).toHaveBeenLastCalledWith(-1);
  });

  it("pulls the row while the scroll is still short of the commit", () => {
    render(<Harness onSwipe={() => {}} />);
    const row = screen.getByTestId("row");

    scroll(row, [
      [20, 0],
      [20, 0],
    ]);

    expect(row).toHaveTextContent("-14");
    expect(row).toHaveAttribute("data-dragging");
  });

  it("turns one market per swipe, however long the momentum coasts", () => {
    const onSwipe = vi.fn();
    render(<Harness onSwipe={onSwipe} />);
    const row = screen.getByTestId("row");

    // One flick: past the threshold, then a long tail of decaying
    // deltas of the kind a Mac trackpad keeps sending.
    scroll(row, [
      [30, 0],
      [40, 0],
      ...Array.from({ length: 20 }, (_, i) => [30 - i, 0] as [number, number]),
    ]);
    expect(onSwipe).toHaveBeenCalledTimes(1);

    // Only a deliberate second swipe, after the deltas stop, pages again.
    act(() => vi.advanceTimersByTime(300));
    scroll(row, [
      [40, 0],
      [40, 0],
    ]);
    expect(onSwipe).toHaveBeenCalledTimes(2);
  });

  it("leaves a vertical scroll to the page, untouched", () => {
    const onSwipe = vi.fn();
    render(<Harness onSwipe={onSwipe} />);

    const events = scroll(screen.getByTestId("row"), [
      [0, 40],
      [6, 90],
    ]);

    expect(onSwipe).not.toHaveBeenCalled();
    // Not merely ignored: the page still gets to scroll.
    expect(events.every((e) => !e.defaultPrevented)).toBe(true);
  });

  it("keeps a horizontal scroll from reaching the browser's back gesture", () => {
    render(<Harness onSwipe={() => {}} />);
    const row = screen.getByTestId("row");

    // Including the momentum after the page has turned, which is the
    // part that would otherwise navigate away mid-coast.
    const events = scroll(row, [
      [40, 0],
      [40, 0],
      [30, 0],
      [10, 0],
    ]);

    expect(events.every((e) => e.defaultPrevented)).toBe(true);
  });

  it("drops the gesture when the pointer is cancelled", () => {
    const onSwipe = vi.fn();
    render(<Harness onSwipe={onSwipe} />);
    const row = screen.getByTestId("row");

    drag(
      row,
      [
        [200, 50],
        [90, 50],
      ],
      { release: false },
    );
    act(() => {
      row.dispatchEvent(
        new PointerEvent("pointercancel", { pointerId: 1, pointerType: "touch", bubbles: true }),
      );
    });

    expect(onSwipe).not.toHaveBeenCalled();
    expect(row).toHaveTextContent("0");
  });
});
