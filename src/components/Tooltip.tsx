import { useRef, useState, type ReactNode } from "react";

const DELAY_MS = 600; // UI_BUILD_GUIDE §0.5: "tooltips on hover after 600ms"

/**
 * Hover tooltip with an explicit 600ms delay.
 *
 * Not `title=""`: the native tooltip's delay is browser-controlled (~1s in
 * Chromium), it cannot be styled, and it does not appear on keyboard focus. The
 * module rail is icon-only, so this label is the only thing naming the control.
 */
export function Tooltip({
  label,
  side = "right",
  children,
}: {
  label: string;
  side?: "right" | "top";
  children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const timer = useRef<number | undefined>(undefined);

  const show = () => {
    clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setOpen(true), DELAY_MS);
  };
  const hide = () => {
    clearTimeout(timer.current);
    setOpen(false);
  };

  const position =
    side === "right"
      ? "left-full top-1/2 -translate-y-1/2 ml-2"
      : "bottom-full left-1/2 -translate-x-1/2 mb-2";

  return (
    <span
      className="relative inline-flex"
      onPointerEnter={show}
      onPointerLeave={hide}
      onFocus={show}
      onBlur={hide}
    >
      {children}
      {open && (
        <span
          role="tooltip"
          data-overlay
          className={`pointer-events-none absolute z-50 whitespace-nowrap border-hairline
                      border-bezel-500 bg-chassis-700 px-2 py-[2px] text-micro
                      text-text-hi shadow-none ${position}`}
        >
          {label}
        </span>
      )}
    </span>
  );
}
