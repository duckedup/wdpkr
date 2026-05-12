
### megagrep

This repo has a semantic codebase index available via `megagrep`. Use it when you need to **orient yourself on a feature area or conceptual question** before making changes — e.g., "where does the commission system live," "how is rate limiting implemented," "what does our PDF generation pipeline look like."

Run `megagrep search "<query>"` and parse the JSON output. The `path` and `summary` fields tell you where to look; read the actual files for ground truth.

**Don't use megagrep for:** exact symbol or text lookup (use `rg`/grep), reading file contents (read files directly), or lookups where you already know the file. megagrep is the conceptual layer; grep is still the right tool when you know what string you're searching for.
