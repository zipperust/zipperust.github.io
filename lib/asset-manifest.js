/**
 * Logical wasm loader names → paths inside a stock Zipper.pdx (build 1.10).
 * Nested Launcher / Music / Fonts paths match a stock Zipper.pdx layout.
 *
 * NOTE: logical names are not unique across kinds (`key`, `parry`, `shuriken`
 * are both images and sounds), so the BYOA cache keys every entry by
 * `${kind}:${name}` — see lib/asset-cache.js.
 */

/** @typedef {'pdi'|'pdt'|'pda'|'pft'} AssetKind */

/**
 * @typedef {{
 *   name: string,
 *   kind: AssetKind,
 *   paths: string[],
 *   required?: boolean,
 * }} AssetEntry
 */

/** @type {AssetEntry[]} */
export const ASSET_MANIFEST = [
  // Images — faces / dialog
  { name: "card", kind: "pdi", paths: ["Images/Launcher/card.pdi", "Images/card.pdi"], required: true },
  { name: "playerface", kind: "pdi", paths: ["Images/playerface.pdi"], required: true },
  { name: "swordface", kind: "pdi", paths: ["Images/swordface.pdi"] },
  { name: "pikeface", kind: "pdi", paths: ["Images/pikeface.pdi"] },
  { name: "ninjaface", kind: "pdi", paths: ["Images/ninjaface.pdi"] },
  { name: "twinstepface", kind: "pdi", paths: ["Images/twinstepface.pdi"] },
  { name: "kingface", kind: "pdi", paths: ["Images/kingface.pdi"] },
  { name: "spiritface", kind: "pdi", paths: ["Images/spiritface.pdi"] },
  { name: "monkface", kind: "pdi", paths: ["Images/monkface.pdi"] },
  { name: "faceblood", kind: "pdi", paths: ["Images/faceblood.pdi"] },
  { name: "dialogbg", kind: "pdt", paths: ["Images/dialogbg.pdt"] },
  { name: "continue", kind: "pdt", paths: ["Images/continue.pdt"] },

  // Props / HUD icons
  { name: "key", kind: "pdi", paths: ["Images/key.pdi"], required: true },
  { name: "chest", kind: "pdi", paths: ["Images/chest.pdi"], required: true },
  { name: "passicon", kind: "pdi", paths: ["Images/passicon.pdi"], required: true },
  { name: "moveicon", kind: "pdi", paths: ["Images/moveicon.pdi"], required: true },
  { name: "centerdot", kind: "pdi", paths: ["Images/centerdot.pdi"], required: true },
  { name: "killicon", kind: "pdi", paths: ["Images/killicon.pdi"], required: true },
  { name: "stabicon", kind: "pdi", paths: ["Images/stabicon.pdi"], required: true },
  { name: "exiticon", kind: "pdt", paths: ["Images/exiticon.pdt"], required: true },

  // Core tables
  { name: "tiles", kind: "pdt", paths: ["Images/unifiedtiles.pdt"], required: true },
  { name: "ninja", kind: "pdt", paths: ["Images/ninja.pdt"] },
  { name: "player", kind: "pdt", paths: ["Images/player.pdt"], required: true },
  { name: "enemy", kind: "pdt", paths: ["Images/enemy.pdt"], required: true },
  { name: "pikeman", kind: "pdt", paths: ["Images/pikeman.pdt"] },
  { name: "piketip", kind: "pdt", paths: ["Images/piketip.pdt"] },
  { name: "twinstep", kind: "pdt", paths: ["Images/twinstep.pdt"] },
  { name: "parry", kind: "pdt", paths: ["Images/parry.pdt"] },
  { name: "shuriken", kind: "pdt", paths: ["Images/shuriken.pdt"] },
  { name: "king", kind: "pdt", paths: ["Images/king.pdt"] },
  // Lua `spiritTable = Images/demon` — Spirit body art.
  { name: "demon", kind: "pdt", paths: ["Images/demon.pdt"] },
  // Spirit-room `door` class (`Images/door` / `doorTable`).
  { name: "door", kind: "pdt", paths: ["Images/door.pdt"] },
  { name: "leftdoor", kind: "pdt", paths: ["Images/leftdoor.pdt"], required: true },
  { name: "rightdoor", kind: "pdt", paths: ["Images/rightdoor.pdt"], required: true },
  { name: "smoke", kind: "pdt", paths: ["Images/smoke.pdt"] },
  { name: "smoke2", kind: "pdt", paths: ["Images/smoke2.pdt"] },
  { name: "trail", kind: "pdt", paths: ["Images/trail.pdt"] },
  { name: "espray1", kind: "pdt", paths: ["Images/espray1.pdt"] },
  { name: "espray2", kind: "pdt", paths: ["Images/espray2.pdt"] },
  { name: "espray3", kind: "pdt", paths: ["Images/espray3.pdt"] },
  { name: "espray4", kind: "pdt", paths: ["Images/espray4.pdt"] },
  { name: "espray5", kind: "pdt", paths: ["Images/espray5.pdt"] },
  { name: "floorspray", kind: "pdt", paths: ["Images/floorspray.pdt"] },
  { name: "hereblood", kind: "pdi", paths: ["Images/hereblood.pdi"] },
  { name: "dripsC", kind: "pdt", paths: ["Images/dripsC.pdt"] },
  { name: "dripsN", kind: "pdt", paths: ["Images/dripsN.pdt"] },
  { name: "dripsS", kind: "pdt", paths: ["Images/dripsS.pdt"] },
  { name: "dripsE", kind: "pdt", paths: ["Images/dripsE.pdt"] },
  { name: "dripsW", kind: "pdt", paths: ["Images/dripsW.pdt"] },
  { name: "isospray", kind: "pdt", paths: ["Images/isospray.pdt"] },

  // Enemy ghosts (spirit rooms)
  { name: "enemy_ghost", kind: "pdt", paths: ["Images/enemy_ghost.pdt"] },
  { name: "ninja_ghost", kind: "pdt", paths: ["Images/ninja_ghost.pdt"] },
  { name: "pikeman_ghost", kind: "pdt", paths: ["Images/pikeman_ghost.pdt"] },
  { name: "twinstep_ghost", kind: "pdt", paths: ["Images/twinstep_ghost.pdt"] },

  // HUD
  { name: "movebar", kind: "pdi", paths: ["Images/movebar.pdi"] },
  { name: "lifebar", kind: "pdi", paths: ["Images/lifebar.pdi"], required: true },
  { name: "readyword", kind: "pdi", paths: ["Images/readyword.pdi"] },
  { name: "pressx", kind: "pdi", paths: ["Images/pressx.pdi"] },
  { name: "restart", kind: "pdi", paths: ["Images/restart.pdi"] },
  { name: "bennett", kind: "pdi", paths: ["Images/bennett.pdi"] },
  { name: "zip", kind: "pdt", paths: ["Images/zip.pdt"] },
  { name: "barmatte", kind: "pdi", paths: ["Images/barmatte.pdi"] },
  { name: "readysegwalk", kind: "pdi", paths: ["Images/readysegwalk.pdi"] },
  { name: "readysegwalk_0", kind: "pdi", paths: ["Images/readysegwalk_0.pdi"] },
  { name: "readysegwalk_1", kind: "pdi", paths: ["Images/readysegwalk_1.pdi"] },
  { name: "readysegkill", kind: "pdi", paths: ["Images/readysegkill.pdi"] },
  { name: "readysegghost1", kind: "pdi", paths: ["Images/readysegghost1.pdi"] },
  { name: "readysegghost2", kind: "pdi", paths: ["Images/readysegghost2.pdi"] },
  { name: "crankhint", kind: "pdt", paths: ["Images/crankhint.pdt"] },
  { name: "hourglass", kind: "pdt", paths: ["Images/hourglass.pdt"] },

  // Ending / score screen (`winbackground`, `credits`, `highscoretable`)
  { name: "endbg", kind: "pdi", paths: ["Images/endbg.pdi"] },
  { name: "highscore", kind: "pdt", paths: ["Images/highscore.pdt"] },
  { name: "wipe", kind: "pdt", paths: ["Images/wipe.pdt"] },

  // Fonts
  {
    name: "headerwhite",
    kind: "pft",
    paths: ["Fonts/headerwhite.pft", "Images/headerwhite.pft"],
    required: true,
  },
  { name: "monoblack", kind: "pft", paths: ["Fonts/monoblack.pft", "Images/monoblack.pft"] },

  // Sounds (logical names match wasm soundm keys)
  { name: "select", kind: "pda", paths: ["Sounds/select.pda"], required: true },
  { name: "buzz", kind: "pda", paths: ["Sounds/buzz.pda"], required: true },
  { name: "slash", kind: "pda", paths: ["Sounds/slash.pda"], required: true },
  { name: "step", kind: "pda", paths: ["Sounds/step.pda"], required: true },
  { name: "swoosh", kind: "pda", paths: ["Sounds/swoosh.pda"] },
  { name: "falldead", kind: "pda", paths: ["Sounds/falldead.pda"] },
  { name: "falldead2", kind: "pda", paths: ["Sounds/falldead2.pda"] },
  { name: "falldead3", kind: "pda", paths: ["Sounds/falldead3.pda"] },
  { name: "falldead4", kind: "pda", paths: ["Sounds/falldead4.pda"] },
  { name: "playerdeath", kind: "pda", paths: ["Sounds/playerdeath.pda"] },
  {
    name: "deathmusic",
    kind: "pda",
    paths: ["Sounds/Music/music_death.pda", "Sounds/music_death.pda"],
  },
  { name: "blood", kind: "pda", paths: ["Sounds/blood.pda"] },
  { name: "lifedown", kind: "pda", paths: ["Sounds/lifedown.pda"] },
  { name: "lifeup", kind: "pda", paths: ["Sounds/lifeup.pda"] },
  { name: "clunk", kind: "pda", paths: ["Sounds/trapdoor.pda"] },
  { name: "key", kind: "pda", paths: ["Sounds/key.pda"] },
  { name: "click", kind: "pda", paths: ["Sounds/click.pda"] },
  { name: "zzt", kind: "pda", paths: ["Sounds/tap-zipper2.pda"] },
  { name: "warning", kind: "pda", paths: ["Sounds/warning.pda"] },
  { name: "parry", kind: "pda", paths: ["Sounds/parry.pda"] },
  { name: "shuriken", kind: "pda", paths: ["Sounds/shuriken.pda"] },
  // soundm.spiritdispel / spiritrevive (`Sounds/ghost3` / `ghost1`).
  { name: "spiritdispel", kind: "pda", paths: ["Sounds/ghost3.pda"] },
  { name: "spiritrevive", kind: "pda", paths: ["Sounds/ghost1.pda"] },
  { name: "transition", kind: "pda", paths: ["Sounds/transition.pda"] },
  { name: "transition2", kind: "pda", paths: ["Sounds/transition2.pda"] },
];

/** Intro music (introchord notes) lives in `Globals.luac`; core extracts it. */
export const INTRO_LUAC_PATHS = ["Globals.luac", "globals.luac"];

/** Victory tune — optional; silent when the user's .pdx lacks it. */
export const SHO_MIDI_PATHS = ["Sounds/Sho.mid", "Sounds/sho.mid", "Sho.mid", "sho.mid"];

export function requiredAssets() {
  return ASSET_MANIFEST.filter((e) => e.required);
}
