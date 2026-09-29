fn main() {
  let m = zipper_core::WorldMap::from_path(zipper_core::WorldMap::data_file_path()).unwrap();
  let room = zipper_core::level::load_start_room(&m);
  println!("start room enemies: {}", room.enemies.len());
  for e in &room.enemies {
    println!("  {:?} @ {},{} marker={}", e.kind, e.x, e.y, m.enemy_marker(e.x,e.y));
  }
  // after expanding to 24
  let room2 = zipper_core::level::viewport_around(&m, zipper_core::worldmap::START_CAMERA, 24, 24);
  println!("±24 enemies: {}", room2.enemies.len());
  for e in &room2.enemies {
    println!("  {:?} @ {},{}", e.kind, e.x, e.y);
  }
}
