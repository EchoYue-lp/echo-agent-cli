# echo-agent-cli semantic baseline

This directory records repository-scoped semantic facts for EKO application
conversation collaboration. The initial baseline is intentionally open: it
classifies the whole repository while modeling only the Conversation,
AgentRouter, foreground-turn, deletion, and surface boundaries needed by the
Side Conversation outcome.

Open closure means unmodeled capabilities remain explicit work, not that the
repository has no defects. High-risk changes start with semantic preflight,
map their first diff, and verify the affected boundary before completion.
