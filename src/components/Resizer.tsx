import { useCallback, useRef } from "react";

type Props = {
  /** Current width of the panel being resized, in px. */
  value: number;
  min: number;
  max: number;
  onChange: (px: number) => void;
  /** Which side of the handle the panel sits on. Determines drag polarity. */
  side: "left" | "right";
  label: string;
};

const STEP = 8;
const STEP_COARSE = 32;

/**
 * A 4px visible rail with a 12px invisible hit area (UI_BUILD_GUIDE Step 3).
 *
 * The hit area is an absolutely-positioned overlay that bleeds 4px past each
 * edge, so the grid column stays exactly 4px and no layout shifts when the
 * pointer enters. Pointer capture is used rather than window listeners: it
 * survives the pointer leaving the element and needs no cleanup on unmount.
 *
 * Keyboard-resizable because "every action reachable without a mouse" (§0.8) is
 * a quality floor, and a drag handle is the easiest place to forget it.
 */
export function Resizer({ value, min, max, onChange, side, label }: Props) {
  const drag = useRef<{ x: number; start: number } | null>(null);

  const onPointerDown = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      if (e.button !== 0) return;
      drag.current = { x: e.clientX, start: value };
      e.currentTarget.setPointerCapture(e.pointerId);
    },
    [value],
  );

  const onPointerMove = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      const d = drag.current;
      if (!d) return;
      const delta = side === "left" ? e.clientX - d.x : d.x - e.clientX;
      onChange(d.start + delta);
    },
    [onChange, side],
  );

  const onPointerUp = useCallback(() => {
    drag.current = null;
  }, []);

  const onKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLDivElement>) => {
      const step = e.shiftKey ? STEP_COARSE : STEP;
      const grow = side === "left" ? "ArrowRight" : "ArrowLeft";
      const shrink = side === "left" ? "ArrowLeft" : "ArrowRight";
      if (e.key === grow) onChange(value + step);
      else if (e.key === shrink) onChange(value - step);
      else if (e.key === "Home") onChange(min);
      else if (e.key === "End") onChange(max);
      else return;
      e.preventDefault();
    },
    [max, min, onChange, side, value],
  );

  return (
    <div className="relative w-[4px] bg-chassis-900">
      <div
        role="separator"
        aria-orientation="vertical"
        aria-label={label}
        aria-valuenow={value}
        aria-valuemin={min}
        aria-valuemax={max}
        tabIndex={0}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onKeyDown={onKeyDown}
        className="absolute inset-y-0 -left-[4px] -right-[4px] cursor-col-resize
                   transition-colors duration-hover hover:bg-bezel-500
                   focus-visible:bg-amber-500"
      />
    </div>
  );
}
