import "mineflayer";

// mineflayer-pathfinder augments Bot at runtime; we load it via require, so declare the field.
declare module "mineflayer" {
  interface Bot {
    pathfinder: any;
  }
}
