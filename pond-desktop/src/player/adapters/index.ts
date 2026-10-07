// The registry of services the player can drive. Adding one is a file here and a line below; the
// bridge, the host and the extension already speak only in the player's own terms.

import type { PlayerAdapter } from "../types";
import { AppleMusicKitAdapter, loadMusicKitFromApple } from "./appleMusicKit";
import { SpotifyWebPlaybackAdapter, loadSpotifySdk } from "./spotifyWebPlayback";
import { SPOTIFY_LOGO_URL } from "../brand";

export interface AdapterContext {
  /** Apple: a developer token the pond signs, since the pond holds the key. */
  fetchDeveloperToken(): Promise<string>;
  /**
   * Whether the pond's network setting lets this page reach `url`: null when it does, else the
   * reason in words for a person. Asked before a service's script is loaded.
   */
  networkAllows(url: string): Promise<string | null>;
  /** Spotify: the person's own access token, held by the pond. `refresh` asks for a new one. */
  fetchUserToken(service: string, refresh: boolean): Promise<string>;
}

/** Services supported by the player, in display order. */
export function knownServices(): string[] {
  return ["apple", "spotify"];
}

/** The adapter for `service`, or null when the player has none. */
export function createAdapter(
  service: string,
  ctx: AdapterContext,
): PlayerAdapter | null {
  // The URL supplies service. Explicit branches keep it out of method selection entirely.
  switch (service) {
    case "apple":
      return new AppleMusicKitAdapter({
        loadMusicKit: loadMusicKitFromApple,
        fetchDeveloperToken: ctx.fetchDeveloperToken,
        networkAllows: ctx.networkAllows,
      });
    case "spotify":
      return new SpotifyWebPlaybackAdapter(
        {
          loadSdk: loadSpotifySdk,
          fetchUserToken: (refresh) => ctx.fetchUserToken("spotify", refresh),
          networkAllows: ctx.networkAllows,
        },
        { logoUrl: SPOTIFY_LOGO_URL },
      );
    default:
      return null;
  }
}
