# Skills Manager

Skills Manager manages reusable agent skills across libraries, runtime environments, and agent-specific skill directories.

## Language

**Skill**:
A reusable capability packaged as a directory that an agent can load.

**Primary Library**:
The Windows-owned Skills Manager skill collection that acts as the source of truth for managed skills.
_Avoid_: Windows repo, master repo, central repo

**Library Replica**:
A local copy of the Primary Library inside another runtime environment.
_Avoid_: WSL repo, remote repo, agent repo

**Runtime Environment**:
An operating environment where agents read skills, such as Windows or a WSL distribution.
_Avoid_: platform, side, machine

**Agent**:
A coding tool that can load skills from one or more skill directories.
_Avoid_: tool, client

**Agent Target**:
A concrete skill directory for one agent inside one runtime environment.
_Avoid_: path variant, agent instance

## Relationships

- A **Primary Library** produces zero or more **Library Replicas**.
- A **Library Replica** belongs to exactly one **Runtime Environment**.
- A **Runtime Environment** contains zero or more **Agent Targets**.
- An **Agent Target** belongs to exactly one **Agent**.
- A **Skill** can be distributed from a **Primary Library** or **Library Replica** to one or more **Agent Targets**.

## Example dialogue

> **Dev:** "When a Windows user enables WSL support, do we link the Windows **Primary Library** directly into WSL **Agent Targets**?"
> **Domain expert:** "No - create a WSL **Library Replica** first, then distribute from that replica to WSL **Agent Targets**."

## Flagged ambiguities

- "agent" was used to mean both **Agent** and **Agent Target** - resolved: the agent is the coding tool, while the target is a concrete skill directory in a runtime environment.
- "repo" was used to mean both **Primary Library** and **Library Replica** - resolved: the primary library is the source of truth, while a replica is an environment-local copy.
