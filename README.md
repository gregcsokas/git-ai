# git-ai   <a href="https://discord.gg/XJStYvkb5U"><img alt="Discord" src="https://img.shields.io/badge/discord-join-5865F2?logo=discord&logoColor=white" /></a>        

<img src="https://github.com/git-ai-project/git-ai/raw/main/assets/docs/git-ai.png" align="right"
     alt="Git AI Logo" width="200" height="200">

Git AI est extensio git fontis aperti quae codicem ab Intelligentia Artificiali generatum in repositoriis tuis persequitur.

Post installationem, automatice omnem lineam ab IA scriptam cum agente, modello, et transcriptis quae eam generaverunt coniungit — ut numquam intentionem, requisita, et decisiones architecturae post codicem tuum amittas.

**Attributio IA in omni commissione:**

`git commit`
```
[hooks-doctor 0afe44b2] wsl compat check
 2 files changed, 81 insertions(+), 3 deletions(-)
you  ██░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░ ai
     6%             mixed   2%             92%
```

**IA Blame ostendit modellum, agentem, et sessionem post omnem lineam:**

`git-ai blame /src/log_fmt/authorship_log.rs`
```bash

cb832b7 (Aidan Cunniffe      2025-12-13 08:16:29 -0500  133) pub fn execute_diff(
cb832b7 (Aidan Cunniffe      2025-12-13 08:16:29 -0500  134)     repo: &Repository,
cb832b7 (Aidan Cunniffe      2025-12-13 08:16:29 -0500  135)     spec: DiffSpec,
cb832b7 (Aidan Cunniffe      2025-12-13 08:16:29 -0500  136)     format: DiffFormat,
cb832b7 (Aidan Cunniffe      2025-12-13 08:16:29 -0500  137) ) -> Result<String, GitAiError> {
fe2c4c8 (claude [session_id] 2025-12-02 19:25:13 -0500  138)     // Resolve commits to get from/to SHAs
fe2c4c8 (claude [session_id] 2025-12-02 19:25:13 -0500  139)     let (from_commit, to_commit) = match spec {
fe2c4c8 (claude [session_id] 2025-12-02 19:25:13 -0500  140)         DiffSpec::TwoCommit(start, end) => {
fe2c4c8 (claude [session_id] 2025-12-02 19:25:13 -0500  141)             // Resolve both commits
fe2c4c8 (claude [session_id] 2025-12-02 19:25:13 -0500  142)             let from = resolve_commit(repo, &start)?;...
```


### Agentes Sustentati

<table>
<tr>
<td align="center" width="20%"><img src="assets/docs/agents/gray/claude_code.png" alt="Claude Code" width="160" /></td>
<td align="center" width="20%"><img src="assets/docs/agents/gray/codex-black.png" alt="Codex" width="160" /></td>
<td align="center" width="20%"><img src="assets/docs/agents/gray/cursor.png" alt="Cursor" width="160" /></td>
<td align="center" width="20%"><img src="assets/docs/agents/gray/copilot.png" alt="GitHub Copilot" width="160" /></td>
<td align="center" width="20%"><img src="assets/docs/agents/gray/opencode.png" alt="OpenCode" width="160" /></td>
</tr>
<tr>
<td align="center"><img src="assets/docs/agents/gray/pi.png" alt="Pi" width="160" /></td>
<td align="center"><img src="assets/docs/agents/gray/windsurf.png" alt="Windsurf" width="160" /></td>
<td align="center"><img src="assets/docs/agents/gray/droid.png" alt="Droid" width="160" /></td>
<td align="center"><img src="assets/docs/agents/gray/amp.png" alt="Amp" width="160" /></td>
<td align="center"><img src="assets/docs/agents/gray/gemini.png" alt="Gemini" width="160" /></td>
</tr>
<tr>
<td align="center"><img src="assets/docs/agents/gray/continue.png" alt="Continue" width="160" /></td>
<td align="center"><img src="assets/docs/agents/gray/junie_white.png" alt="Junie" width="160" /></td>
<td align="center"><img src="assets/docs/agents/gray/rovodev.png" alt="Rovo Dev" width="160" /></td>
<td align="center"><img src="assets/docs/agents/gray/firebender.png" alt="Firebender" width="160" /></td>
<td align="center"><a href="https://usegitai.com/docs/cli/add-your-agent">+ Adde Agentem</a></td>
</tr>
</table>


## Installatio

**Mac, Linux, Windows (WSL)**

```bash
curl -sSL https://usegitai.com/install.sh | bash
```

**Windows (non-WSL)**

Sustentatio Windows non-WSL nunc experimentalis est et sub activa evolutione. Libenter sententias tuas audiemus dum laboramus ut sustentationem Windows non-WSL ad productionem paratam reddamus.

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://usegitai.com/install.ps1 | iex"
```

Id est — **nulla configuratio per repositorium requiritur.** Manda et committe ut soles. Git AI attributionem automatice persequitur.

### Electiones Nostrae
- **Nullae mutationes operis** — Tantum manda et committe. Git AI codicem IA accurate persequitur sine historiam git tuam inquinando.
- **"Detegere" codicem IA est anti-exemplar** — Git AI non coniicit utrum fragmentum ab IA generatum sit. Agentes sustentati exacte referunt quas lineas scripserunt, tibi attributionem quam accuratissimam praebentes.
- **Locale primum** — Plene sine conexione operatur, nullum initium sessionis requiritur.
- **Git nativum et norma aperta** — Git AI [normam apertam](https://github.com/git-ai-project/git-ai/blob/main/specs/git_ai_standard_v3.0.0.md) aedificavit ad codicem ab IA generatum cum Git Notis persequendum.
- **Repositio Mandatorum Secura** — Git AI omnem lineam codicis IA cum mandato quod eam generavit coniungit. Ab v1.0.0 Sessiones Agentium extra Git reponuntur et optionaliter cum [nube](https://usegitai.com/docs/platform/overview) tuae turmae vel repositorio mandatorum [auto-hospite](https://usegitai.com/docs/platform/self-hosting) synchronizari possunt — repositoria levia servantes, accessum granularem permittentes, et impedientes ne PII vel secreta in Git effluant.


<table style="table-layout:fixed; width:100%">
<tr>
<th align="center" width="50%">Solus</th>
<th align="center" width="50%">Pro Turmis</th>
</tr>
<tr>
<td align="center"><img src="https://github.com/git-ai-project/git-ai/blob/main/assets/docs/solo-player.svg" alt="Solus — omnia in machina tua manent" width="400"></td>
<td align="center"><img src="https://github.com/git-ai-project/git-ai/blob/main/assets/docs/for-teams.svg" alt="Pro turmis — contextus communis per turmam tuam" width="400"></td>
</tr>
<tr>
<td valign="top">

- Auctoritas IA in Git Notis reposita, cum indicibus ad transcripta in SQLite locali reposita
- Transcripta solum localiter, in computatro, reposita
- Metire auctoritatem IA per commissiones cum `git-ai stats`

</td>
<td valign="top">

- Auctoritas IA in Git Notis reposita
- Indices ad repositorium transcriptorum nubis vel auto-hospitis cum accessu integrali, redactione secretorum, et filtratione PII
- Agentes et fabri transcripta et summas pro quolibet segmento codicis ab IA generati legere possunt
- Tabulae progredientes trans-agentes ad metiendum adoptionem IA, durabilitatem codicis, et comparandos agentes per turmam tuam

**[Hic preme ut accessum praematurum obtineas](https://calendly.com/d/cxjh-z79-ktm/meeting-with-git-ai-authors)**

</td>
</tr>
</table>



## Statisticae Attributionis

Attributio IA ad lineam permittit te codicem IA per totum SDLC persequi. Persequere quantum codicis IA acceptum, commissum, per recognitionem codicis, et in productionem pervenerit — ut instrumenta et consuetudines optimas identifies.

```bash
git-ai stats --json
git ai stats <start_sha>..<end_sha> --json
```

Calculat % codicis IA, lineas IA generatas contra commissas, rates acceptationis, correctiones humanas per instrumentum et modellum divisas. Plura disce: [Documentatio referentiae mandati Stats](https://usegitai.com/docs/cli/reference#stats).


<details>
<summary>Exemplum exitus JSON</summary>

```json
{
  "human_additions": 28,
  "ai_additions": 76,
  "ai_accepted": 47,
  "git_diff_deleted_lines": 34,
  "git_diff_added_lines": 104,
  "tool_model_breakdown": {
    "claude_code/claude-sonnet-4-5-20250929": {
      "ai_additions": 76,
      "ai_accepted": 47
    }
  }
}
```

</details>

## IA Blame

Git AI blame est substitutum directum pro `git blame` quod attributionem IA pro omni linea ostendit. Omnes [vexilla normalia `git blame`](https://git-scm.com/docs/git-blame) sustinet.

```bash
git-ai blame /src/log_fmt/authorship_log.rs
```

```bash
cb832b7 (Aidan Cunniffe 2025-12-13 08:16:29 -0500  133) pub fn execute_diff(
cb832b7 (Aidan Cunniffe 2025-12-13 08:16:29 -0500  134)     repo: &Repository,
cb832b7 (Aidan Cunniffe 2025-12-13 08:16:29 -0500  135)     spec: DiffSpec,
cb832b7 (Aidan Cunniffe 2025-12-13 08:16:29 -0500  136)     format: DiffFormat,
cb832b7 (Aidan Cunniffe 2025-12-13 08:16:29 -0500  137) ) -> Result<String, GitAiError> {
fe2c4c8 (claude         2025-12-02 19:25:13 -0500  138)     // Resolve commits to get from/to SHAs
fe2c4c8 (claude         2025-12-02 19:25:13 -0500  139)     let (from_commit, to_commit) = match spec {
fe2c4c8 (claude         2025-12-02 19:25:13 -0500  140)         DiffSpec::TwoCommit(start, end) => {
fe2c4c8 (claude         2025-12-02 19:25:13 -0500  141)             // Resolve both commits
fe2c4c8 (claude         2025-12-02 19:25:13 -0500  142)             let from = resolve_commit(repo, &start)?;
fe2c4c8 (claude         2025-12-02 19:25:13 -0500  143)             let to = resolve_commit(repo, &end)?;
fe2c4c8 (claude         2025-12-02 19:25:13 -0500  144)             (from, to)
fe2c4c8 (claude         2025-12-02 19:25:13 -0500  145)         }
```

Supplementa communitatis exstant quae attributionem IA in editoribus popularibus ostendunt, coloribus per sessionem agentis distincta. Supra lineam vola ut mandatum originale vel summam videas.

<table style="table-layout:fixed; width:100%">
<tr>
<th width="35%">Editores Sustentati</th>
<th width="65%"></th>
</tr>
<tr>
<td valign="top">

- [VS Code](https://marketplace.visualstudio.com/items?itemName=git-ai.git-ai-vscode)
- [Cursor](https://marketplace.visualstudio.com/items?itemName=git-ai.git-ai-vscode)
- [Windsurf](https://marketplace.visualstudio.com/items?itemName=git-ai.git-ai-vscode)
- [Antigravity](https://marketplace.visualstudio.com/items?itemName=git-ai.git-ai-vscode)
- [Emacs magit](https://github.com/jwiegley/magit-ai)
- *Sustentationem pro alio editore aedificavisti? [Aperi PR](https://github.com/git-ai-project/git-ai/pulls)*

</td>
<td>
<img width="100%" alt="Extensio Git AI VS Code ostendens attributionem IA coloribus distinctam in margine" src="https://github.com/user-attachments/assets/94e332e7-5d96-4e5c-8757-63ac0e2f88e0" />
</td>
</tr>
</table>


## Intellege Cur cum Arte `/ask`

Aliquid vides quod non intellegis? Ars `/ask` tibi permittit cum agente qui codicem scripsit de instructionibus, decisionibus, et intentione fabri qui opus assignavit colloqui. Git AI artem `/ask` ad `~/.agents/skills/` tempore installationis addit ut ex quolibet agente cum ea colloqui possis.

```
/ask Why didn't we use the SDK here?
```

Agentes cum accessu ad intentionem originalem et codicem fontem "cur" intellegunt. Agentes qui solum codicem legere possunt tibi dicere possunt quid faciat, sed non cur:

| Legens Codicem + Transcriptum (`/ask`) | Solum Legens Codicem (sine Git AI) |
|---|---|
| Cum Aidan telemetriam aedificabat, agenti mandavit ne exitum CLI nostri telemetriam purgando impediat. Loco SDK Sentry directe utendi, exemplar excogitavimus quod eventus localiter primum per `append_envelope()` scribit, deinde eos in fundo per subprocessum separatum purgat. Hoc viam calidam celerem servat et telemetriam asynchrone post factum mittit. | `src/commands/flush_logs.rs` est involucrum 5 linearum quod ad `src/observability/flush.rs` (~700 lineae) delegat. Stratum `commands/` dispatchium CLI tractat; `observability/` Sentry, PostHog, metricas, et processationem diariorum tractat. Moduli paralleli ut `flush_cas`, `flush_logs`, `flush_metrics_db` idem exemplar tenuis-dispatchii sequuntur. |


<details>
<summary>Fac Agentes Tuos Sapientiores</summary>

Agentes pauciores errores faciunt et codicem magis sustentabilem producunt cum requisita et decisiones post codicem quem aedificant intellegunt. Optimus modus hunc contextum praebendi est agentibus idem instrumentum `/ask` quod tu ipse uteris dare. Iube agentes tuos `/ask` in modo planificandi uti:

`Claude|AGENTS.md`
```markdown
- In plan mode, always use the /ask skill to read the code and the original transcript that generated it. Understanding intent will help you write a better plan.
```

</details>


### Quomodo Git AI Operatur
1. **Hami `Edit|Write|Bash`** activantur cum Agentes mutationes in repositorio faciunt
2. **Hami `git-ai checkpoint` vocant** ut omnem lineam Codicis IA cum modello, Agente, et mandato quod eam generavit coniungant.
3. **Post Commissionem** Nota Git cum attributionibus IA commissioni adiungitur
4. **In `merge --squash`, `rebase`, `cherry-pick`, `stash`, `pop`, `commit --amend`, etc** Attributiones IA automatice transferuntur

#### Exemplum Notae
`refs/notes/ai/commit_sha`
```
hooks/post_clone_hook.rs
  prompt_id_123 6-8
  prompt_id_456 16,21,25
main.rs
  prompt_id_123 12-199,215,311
---
...Metadata mandati includens agentem, modellum, et nexum ad transcriptum plenum sessionis
```

Pro pluribus informationibus [recense normam apertam Git AI ad attribuendum codicem IA cum Git Notis](https://github.com/git-ai-project/git-ai/blob/main/specs/git_ai_standard_v3.0.0.md).

## Subsidia

- [Optiones Configurationis](https://usegitai.com/docs/cli/configuration)
- [Referentia CLI](https://usegitai.com/docs/cli/reference)
- [Quomodo impactum agentium codificantium metiaris](https://usegitai.com/how-to-measure-ai-code)


## Pro Turmis

[Git AI Pro Turmis](https://usegitai.com/enterprise) data attributionis ad gradum PR, contributoris, turmae repositorii, et organizationis aggregat:

- **Persequutio pleni cycli vitae** — Vide quantum codicis IA acceptum, commissum, in recognitione rescriptum, et dispositum sit — et utrum alarmas vel incidentes post deploymentum causet.
- **Statisticae turmae et contributorum** — Identifica qui agentes in fundo efficaciter utantur et quid turmae magni ponderis aliter faciant.
- **Paratio agentium** — Metire impactum artium, regularum, MCP, testium, et mutationum `AGENTS.md` per repositoria et genera operum.

### Optiones Dispositionis

Git AI designatum est ut ubicumque organizatio tua fabricandi operatur currat:

- **Auto-hospite (commendatum pro magnis organizationibus)** — Dispone Git AI intra infrastructuram tuam (AWS, VPC, in loco). Plenum imperium datorum, accessus, et integrationum. Optimum pro organizationibus cum strictis requisitis securitatis, conformitatis, vel residentiae datorum.
- **Git AI Nubes** — Hospitium plene administratum a Git AI. Celerior configuratio, nullum onus infrastructurae, et actualizationes automaticae — optimum pro turmis quae celeriter incipere volunt.

Ambae optiones idem modellum attributionis, tabulas, et integrationes sustinent — elige secundum praeferentias securitatis et operationis tuas.

**[Obtine accessum praematurum](https://calendly.com/d/cxjh-z79-ktm/meeting-with-git-ai-authors)**

![new-graphic-dashboards](https://github.com/user-attachments/assets/1e2aec73-4e96-4531-ab5f-fe4deef2bbab)

## Licentia
Apache 2.0
