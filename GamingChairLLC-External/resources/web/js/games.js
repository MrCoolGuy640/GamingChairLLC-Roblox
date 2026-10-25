// ---------------------------------------------
// Games grid
// ---------------------------------------------
const gameList = [
  "Borderland", "Demonfall", "Lucky Blocks", "Blade Ball",
  "Pet Sim 99", "Anime Last Stand", "Blox Fruits", "99 Nights",
];

const gamesGrid = document.getElementById("gamesGrid");
dbg("gamesGrid =", gamesGrid);
gameList.forEach((name) => {
  const tile = document.createElement("button");
  tile.className = "game-tile";
  tile.innerHTML = `
    <span class="game-tile-icon">
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7">
        <rect x="3" y="3" width="7" height="7" rx="1.5"/>
        <rect x="14" y="3" width="7" height="7" rx="1.5"/>
        <rect x="3" y="14" width="7" height="7" rx="1.5"/>
        <rect x="14" y="14" width="7" height="7" rx="1.5"/>
      </svg>
    </span>
    <span class="game-tile-name">${name}</span>
  `;
  tile.addEventListener("click", () => {
    console.log("[DEBUG] open game page:", name);
  });
  gamesGrid.appendChild(tile);
});