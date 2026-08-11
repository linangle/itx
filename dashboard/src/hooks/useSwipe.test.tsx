import { describe, expect, it, vi } from "vitest";
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

function Harness({ onSwipe }: { onSwipe: (direction: SwipeDirection) => void }) {
  const [ref, offset, dragging] = useSwipe<HTMLDivElement>(onSwipe);
  return (
    <div ref={ref} data-testid="row" data-dragging={dragging || undefined}>
      {offset}
    </div>
  );
}

describe("useSwipe", () => {
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
