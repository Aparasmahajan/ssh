import { useMemo } from "react";
import { AppShell } from "./components/AppShell";
import { HostRowGallery } from "./fixtures/HostRowGallery";
import { DEMO_HOSTS, generateHosts } from "./fixtures/hosts";

/**
 * Fixture routing.
 *
 * `visual-check.spec.ts` navigates to `/?fixture=session-view` and
 * `/?fixture=sftp-dual-pane`, so query-param fixtures are the harness's entry
 * mechanism. Extra params for manual work:
 *
 *   ?fixture=host-row-gallery   every HostRow state (Step 4's matrix)
 *   ?hosts=10000                synthetic host count, for the Step 5 budget
 *   ?compact=1                  compact tree rows
 */
export function App() {
  const params = new URLSearchParams(window.location.search);
  const fixture = params.get("fixture");
  const hostCount = Number(params.get("hosts") ?? "0");
  const compact = params.get("compact") === "1";

  const hosts = useMemo(
    () => (hostCount > 0 ? generateHosts(hostCount) : DEMO_HOSTS),
    [hostCount],
  );

  if (fixture === "host-row-gallery") return <HostRowGallery />;

  void compact;
  return <AppShell fallbackHosts={hosts} />;
}
