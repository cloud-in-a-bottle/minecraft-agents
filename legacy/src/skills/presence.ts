import { obj, type Skill, type SkillContext } from "../skillkit.js";

export const skills: Skill[] = [
  {
    tool: {
      name: "who_online",
      description: "List the players currently online from the bot's tab list, with each one's ping.",
      input_schema: obj({}, []),
    },
    run: async (ctx: SkillContext, _input): Promise<string> => {
      const bot = ctx.bot as any;
      try {
        const entries = Object.values(bot.players)
          .filter((p: any) => p?.username && p.username !== bot.username)
          .map((p: any) => {
            const ping = p.ping ?? "?";
            const agent = /^agent_\d+$/i.test(p.username) ? " [agent]" : "";
            return { username: p.username, text: `${p.username} (${ping} ms)${agent}` };
          })
          .sort((a, b) => a.username.localeCompare(b.username, undefined, { numeric: true, sensitivity: "base" }));
        if (entries.length === 0) return "no other players online";
        return `${entries.length} online: ${entries.map((e) => e.text).join(", ")}`;
      } catch (err) {
        return `error: ${(err as Error).message}`;
      }
    },
  },
];
