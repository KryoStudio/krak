// Placeholder library for the browser preview and the "demo data" dev toggle.
import type { Shard } from './api';

const min = 60_000;

function shard(game: string | null, title: string | null, duration: number, sizeMb: number, ageMinutes: number, favorite = false): Shard {
  const name = title ?? `${game ?? 'Desktop'} 2026-10-28 21-03-${String(ageMinutes % 60).padStart(2, '0')}`;
  const id = game ? `${game}/${name}.mp4` : `${name}.mp4`;
  return { id, path: '', title, game, duration, size: sizeMb * 1_000_000, createdAt: Date.now() - ageMinutes * min, favorite };
}

export function demoShards(): Shard[] {
  return [
    shard('Rocket League', 'Ceiling shot', 30, 48, 6, true),
    shard('GTA V', null, 45, 71, 42),
    shard('Fortnite', 'Victoire royale', 30, 52, 95, true),
    shard('Rocket League', null, 20, 31, 180),
    shard('GTA V', 'Course-poursuite sur le pont', 60, 96, 300),
    shard('Fortnite', null, 30, 49, 1500),
    shard(null, null, 15, 12, 2900),
    shard('Rocket League', 'Triple save', 30, 47, 4400, true),
    shard('GTA V', null, 30, 50, 7300),
    shard('Valorant', 'Ace en défense', 40, 63, 9800),
    shard('Valorant', null, 25, 38, 12000),
  ];
}
