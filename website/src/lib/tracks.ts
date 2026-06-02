// ─────────────────────────────────────────────────────────────────────────────
// TRACKS SSOT — the four persona tracks that group the 16 journeys.
//
// Navigation and the self-select landing use these four tracks instead of 14
// flat journeys (paradox of choice). Each track owns a slice of the journey
// backbone and leads with the pillars it exercises most.
// ─────────────────────────────────────────────────────────────────────────────
import type { PillarId } from './positioning';
import { type Journey, type TrackId, getJourneysForTrack } from './journeys';

export interface Track {
  id: TrackId;
  /** Short nav/label title. */
  title: string;
  /** First-person persona used on the self-select cards ("I am…"). */
  persona: string;
  /** One-line value promise. */
  tagline: string;
  pillars: PillarId[];
  /** Inline SVG path for the track icon. */
  iconPath: string;
}

export const tracks: Track[] = [
  {
    id: 'get-started',
    title: 'Get Started',
    persona: "I'm installing for the first time",
    tagline: 'From zero to compressed reads in ten minutes — auto-detected for your editor.',
    pillars: ['perceive', 'compress'],
    iconPath: 'M13 10V3L4 14h7v7l9-11h-7z',
  },
  {
    id: 'daily-workflow',
    title: 'Daily Workflow',
    persona: 'I code with my AI every day',
    tagline: 'Compressed reads, persistent memory and smart routing on every turn.',
    pillars: ['compress', 'remember', 'route'],
    iconPath: 'M12 6V3m0 18v-3m6-6h3M3 12h3m11.196-4.196 2.121-2.121M5.515 18.485l2.121-2.121m9.849 0 2.121 2.121M5.515 5.515 7.636 7.636M16 12a4 4 0 1 1-8 0 4 4 0 0 1 8 0Z',
  },
  {
    id: 'scale-teams',
    title: 'Scale & Teams',
    persona: "We're scaling to multiple agents and a team",
    tagline: 'Shared memory, providers and a team server — one brain across many agents.',
    pillars: ['remember', 'route'],
    iconPath: 'M18 18.72a9.094 9.094 0 0 0 3.741-.479 3 3 0 0 0-4.682-2.72m.94 3.198.001.031c0 .225-.012.447-.037.666A11.944 11.944 0 0 1 12 21c-2.17 0-4.207-.576-5.963-1.584A6.062 6.062 0 0 1 6 18.719m12 0a5.971 5.971 0 0 0-.941-3.197m0 0A5.995 5.995 0 0 0 12 12.75a5.995 5.995 0 0 0-5.058 2.772m0 0a3 3 0 0 0-4.681 2.72 8.986 8.986 0 0 0 3.74.477m.94-3.197a5.971 5.971 0 0 0-.94 3.197M15 6.75a3 3 0 1 1-6 0 3 3 0 0 1 6 0Zm6 3a2.25 2.25 0 1 1-4.5 0 2.25 2.25 0 0 1 4.5 0Zm-13.5 0a2.25 2.25 0 1 1-4.5 0 2.25 2.25 0 0 1 4.5 0Z',
  },
  {
    id: 'operate-govern',
    title: 'Operate & Govern',
    persona: 'We run it in production, safely',
    tagline: 'Security, roles, budgets and performance tuning for real codebases.',
    pillars: ['govern'],
    iconPath: 'M9 12.75 11.25 15 15 9.75m-3-7.036A11.959 11.959 0 0 1 3.598 6 11.99 11.99 0 0 0 3 9.749c0 5.592 3.824 10.29 9 11.623 5.176-1.332 9-6.03 9-11.622 0-1.31-.21-2.571-.598-3.751h-.152c-3.196 0-6.1-1.248-8.25-3.285Z',
  },
];

export function getTrack(id: TrackId): Track {
  const t = tracks.find((x) => x.id === id);
  if (!t) throw new Error(`Unknown track: ${id}`);
  return t;
}

/** Returns each track paired with its ordered journeys — drives nav + landing. */
export function getTracksWithJourneys(): Array<Track & { journeys: Journey[] }> {
  return tracks.map((t) => ({ ...t, journeys: getJourneysForTrack(t.id) }));
}
