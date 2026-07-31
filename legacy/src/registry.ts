import type { BehaviorHandler, Skill } from "./skillkit.js";
import { skills as ironSkills } from "./skills_iron.js";
import { skills as memorySkills } from "./skills_memory.js";
import { skills as survivalSkills, behaviors as survivalBehaviors } from "./skills_survival.js";
import { skills as multiSkills } from "./skills_multiagent.js";
import { skills as presenceSkills } from "./skills/presence.js";
import { skills as messagingSkills } from "./skills/messaging.js";
import { skills as ruleSkills } from "./skills/rules.js";

// Aggregated in a leaf module so skillkit stays free of a circular import.
export const ALL_SKILLS: Skill[] = [
  ...ironSkills,
  ...memorySkills,
  ...survivalSkills,
  ...multiSkills,
  ...presenceSkills,
  ...messagingSkills,
  ...ruleSkills,
];
export const ALL_BEHAVIORS: BehaviorHandler[] = [...survivalBehaviors];
