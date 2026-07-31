import { obj, type Skill, type SkillContext } from "../skillkit.js";

export const skills: Skill[] = [
  {
    tool: {
      name: "message",
      description:
        "Privately message your owner (in-game /msg) or a teammate agent owned by the same player. The ONLY way this bot can talk to anyone.",
      input_schema: obj({ to: { type: "string" }, message: { type: "string" } }, ["to", "message"]),
    },
    run: async (ctx: SkillContext, input): Promise<string> => {
      try {
        const owner = ctx.self.owner;
        if (owner !== null && input.to === owner) {
          (ctx.bot as any).whisper(owner, input.message);
          return `messaged owner ${owner}`;
        }
        if (owner !== null && ctx.peers.ownerOf(input.to) === owner) {
          const ok = ctx.peers.send(input.to, ctx.self.username, input.message);
          return ok ? `delivered to ${input.to}` : `${input.to} is offline`;
        }
        return `can only message your owner or a teammate agent (owned by ${owner ?? "nobody"})`;
      } catch (err) {
        return `error: ${(err as Error).message}`;
      }
    },
  },
  {
    tool: {
      name: "message_team",
      description: "Broadcast an in-process message to every fellow agent owned by the same player.",
      input_schema: obj({ message: { type: "string" } }, ["message"]),
    },
    run: async (ctx: SkillContext, input): Promise<string> => {
      try {
        const mates = ctx.peers.teammates(ctx.self.owner).filter((n) => n !== ctx.self.username);
        if (mates.length === 0) return "no teammates online";
        const sent = mates.filter((mate) => ctx.peers.send(mate, ctx.self.username, input.message));
        return `sent to ${sent.length} teammate(s): ${sent.join(", ")}`;
      } catch (err) {
        return `error: ${(err as Error).message}`;
      }
    },
  },
];
