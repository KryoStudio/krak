// Visual identity of a game until real icons are extracted: two tint colors and initials.

const known: Record<string, [string, string]> = {
  'GTA V': ['#ff7a45', '#7b2cbf'],
  'Rocket League': ['#2d7dff', '#ff8a1f'],
  Fortnite: ['#8a3cff', '#2fd3ff'],
  Valorant: ['#ff4655', '#3a1f5c'],
};

function hash(s: string): number {
  let h = 0;
  for (const c of s) h = (h * 31 + c.charCodeAt(0)) | 0;
  return Math.abs(h);
}

export function gameTint(game: string | null): [string, string] {
  if (!game) return ['#2a2f38', '#4a5566'];
  if (known[game]) return known[game];
  const hue = hash(game) % 360;
  return [`hsl(${hue} 70% 55%)`, `hsl(${(hue + 60) % 360} 60% 35%)`];
}

/** "GTA V" → "GTA", "Rocket League" → "RL", "Fortnite" → "F". */
export function gameInitials(game: string): string {
  const words = game.split(/[\s:_-]+/).filter(Boolean);
  if (/^[A-Z0-9]{2,3}$/.test(words[0])) return words[0];
  return (words.length > 1 ? words[0][0] + words[1][0] : game[0]).toUpperCase();
}
