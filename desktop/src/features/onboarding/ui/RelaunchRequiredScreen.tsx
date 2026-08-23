import { RecoveryScreen } from "./RecoveryScreen";

export function RelaunchRequiredScreen() {
  return (
    <RecoveryScreen
      testId="relaunch-required"
      title="Restart ST8WRX to finish recovery"
      body="Your identity was updated. ST8WRX needs to restart so syncing and agents run under it."
    />
  );
}
