// Placeholder data for the UI preview, until the capture engine is wired in.

export type GameId = 'gta' | 'rl' | 'fortnite' | 'desktop';

export interface Game {
  id: GameId;
  name: string;
  /** two colors for the placeholder thumbnail */
  tint: [string, string];
}

export interface Shard {
  id: number;
  game: GameId;
  title: string | null;
  duration: number;
  sizeMb: number;
  ageMinutes: number;
  favorite: boolean;
}

export const games: Record<GameId, Game> = {
  gta: { id: 'gta', name: 'GTA V', tint: ['#ff7a45', '#7b2cbf'] },
  rl: { id: 'rl', name: 'Rocket League', tint: ['#2d7dff', '#ff8a1f'] },
  fortnite: { id: 'fortnite', name: 'Fortnite', tint: ['#8a3cff', '#2fd3ff'] },
  desktop: { id: 'desktop', name: '', tint: ['#2a3550', '#4fc3f7'] },
};

export const shards: Shard[] = [
  { id: 1, game: 'rl', title: 'Ceiling shot', duration: 30, sizeMb: 48, ageMinutes: 6, favorite: true },
  { id: 2, game: 'gta', title: null, duration: 45, sizeMb: 71, ageMinutes: 42, favorite: false },
  { id: 3, game: 'fortnite', title: 'Victoire royale', duration: 30, sizeMb: 52, ageMinutes: 95, favorite: true },
  { id: 4, game: 'rl', title: null, duration: 20, sizeMb: 31, ageMinutes: 180, favorite: false },
  { id: 5, game: 'gta', title: 'Course-poursuite sur le pont', duration: 60, sizeMb: 96, ageMinutes: 300, favorite: false },
  { id: 6, game: 'fortnite', title: null, duration: 30, sizeMb: 49, ageMinutes: 1500, favorite: false },
  { id: 7, game: 'desktop', title: null, duration: 15, sizeMb: 12, ageMinutes: 2900, favorite: false },
  { id: 8, game: 'rl', title: 'Triple save', duration: 30, sizeMb: 47, ageMinutes: 4400, favorite: true },
  { id: 9, game: 'gta', title: null, duration: 30, sizeMb: 50, ageMinutes: 7300, favorite: false },
];
