import type { Connection } from "@/types";

/**
 * A port chip: one connection type on a host's faceplate.
 *
 * §0.1 — "the host row **is** a faceplate, and each connection type is a port
 * chip on it." §0.6 — "Each chip is a click target that opens that connection
 * type directly. This is the whole product thesis made clickable."
 *
 * §0.6 spec: 11px Plex Mono uppercase, 3px radius, 1px bezel border, transparent
 * fill. Hover: amber-900 fill, amber-500 border and text.
 */
export function PortChip({
  connection,
  onOpen,
  tabIndex,
  chipRef,
}: {
  connection: Connection;
  onOpen: () => void;
  tabIndex: number;
  chipRef?: (el: HTMLButtonElement | null) => void;
}) {
  const { kind, identityName, readOnly } = connection;

  // §0.6: "Named SFTP identities render as SFTP·deploy. The PRD's
  // multi-identity-per-host feature is invisible unless the UI shows the
  // identity name, so it always does."
  const label = identityName ? `${kind}·${identityName}` : kind;
  const aria = identityName
    ? `Open ${kind} on ${identityName} identity${readOnly ? " (read-only)" : ""}`
    : `Open ${kind}`;

  return (
    <button
      ref={chipRef}
      type="button"
      tabIndex={tabIndex}
      aria-label={aria}
      onClick={(e) => {
        e.stopPropagation(); // a chip click opens a connection, it does not just select the row
        onOpen();
      }}
      className="num rounded border-hairline border-bezel-500 bg-transparent px-[5px]
                 uppercase text-micro leading-[14px] text-text-mid
                 transition-colors duration-hover
                 hover:border-amber-500 hover:bg-amber-900 hover:text-amber-500"
      style={{ letterSpacing: "var(--tracking-chip)" }}
    >
      {label}
      {/* Read-only is stated, not implied by absence. Step 10: the user should
          see that mutating actions exist and that this identity can't do them. */}
      {readOnly && <span aria-hidden> ·ro</span>}
    </button>
  );
}
