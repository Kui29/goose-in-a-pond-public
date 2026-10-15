import { InkProvider } from "@jarida/ink/react";

import { useAppState, useAppDispatch } from "./state/useAppContext";
import { GuiMode } from "./modes/GuiMode";
import { VoiceMode } from "./modes/voice";
import { OnboardingWizard } from "./components/onboarding";
import { SignInRequired } from "./components/SignInRequired";
import { api } from "./api/PondApiClient";
import { useTheme } from "./hub/state/themeStore";

/**
 * Ink in `context` mode (theme only, no CSS writes): `themeStore` already set the properties on
 * <html> before first paint, and passing its own theme object keeps DOM and components in sync.
 */
function InkScope({ children }: { children: React.ReactNode }) {
  const { ink } = useTheme();
  return (
    <InkProvider mode="context" theme={ink}>
      {children}
    </InkProvider>
  );
}

export function App() {
  const { mode, needsOnboarding, signInRequired } = useAppState();
  const dispatch = useAppDispatch();

  if (signInRequired) {
    return (
      <InkScope>
        <SignInRequired />
      </InkScope>
    );
  }

  if (needsOnboarding) {
    return (
      <InkScope>
        <OnboardingWizard
        onComplete={async () => {
          await api.completeOnboarding();
          dispatch({ type: "SET_NEEDS_ONBOARDING", payload: false });
        }}
        />
      </InkScope>
    );
  }

  return <InkScope>{mode === "voice" ? <VoiceMode /> : <GuiMode />}</InkScope>;
}
