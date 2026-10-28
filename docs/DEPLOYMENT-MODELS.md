# Deployment models

Timber's shape should follow who owns the decision to clean up. The capture engine stays local in every model, because it reads uncommitted work (working files, ignored files, sometimes secrets) and because its refusals ("source changed during capture", "nested repository", "store must be private") are facts about the machine holding the source. What can travel is the output: manifests, summaries and reports, which carry paths and hashes, never contents.

| Model | What hurts | Shape | Stays local | What is shared |
|---|---|---|---|---|
| Single developer with coding agents | dozens of worktrees nobody remembers; disk; uncommitted work in volatile folders | the utility, run by the person or their agent: inspect, checkpoint, verify, with the [agent operator guide](AGENT-OPERATORS.md) | everything | nothing |
| Small team | the same, per person; one person's worktree holds another's fix; no agreed place to preserve before cleanup | the utility plus a shared store on a mounted volume or bucket the team already has, and one convention file in the repository naming the store, the standing exclusions and the schedule (proposed: [team convention](PRD-TEAM-CONVENTION.md)) | capture, verify | the store, the convention |
| Department (many teams, many repositories) | nobody sees the sprawl; cleanup has no owner; audit asks where uncommitted work went | a scheduled read-only `inspect` per repository, summaries pushed to one place, alarms on dirty or volatile counts, cleanup gated per worktree by a person | capture and checkpoint on each machine; the schedule only inspects and reports | inspect summaries and reports |
| Corporate (hundreds of repositories, compliance) | provenance of uncommitted work is a control question; retention and legal hold; many identities | a service around the scheduled inspect: inventory, retention policy, access control, evidence export for audit; the engine remains a local binary the service orchestrates | capture, checkpoint, verify, restore, on the employee's machine or a runner inside the network | manifests, verification receipts, reports |

Three consequences:

- **No hosted capture.** Timber will not offer a cloud service that receives worktree contents. A team or company that wants central visibility runs the schedule on its own machines and ships the output.
- **Reports are the product boundary.** Everything a dashboard, an auditor or a manager needs is already in the JSON and HTML output, which is why those formats are versioned by schema number.
- **Order of work.** The single-developer model is what the crate serves today. The team convention is one small addition. The department model is the first one anyone would pay for, and it reuses the same read-only scheduling pattern as any other monitor. The corporate model waits for a customer.
