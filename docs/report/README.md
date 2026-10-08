# Course Report Materials

This directory only stores the inputs and images required to write `report.docx`; it does not serve as a project implementation specification. For the project's current architecture, see [C1](../architecture/system-context.md) and [C2](../architecture/containers.md); for development and usage, see the root [README](../../README.md).

The current project name is `bak`. This directory retains the Data Backup course report snapshot from before the rename (at the time PR #5 was merged), including the single-server skeleton, startup parameters, and authentication status API from that time. The project has now removed the product implementation and is starting paired development from the C1 and C2 designs; therefore, the "implemented" markers in the report do not represent the current code state. After reimplementation, the report will be synchronized according to actual progress; for now, the structured model, images, and local Word files are retained for requirements and historical design reference.

- `use-cases.yaml`: source for the actors, use cases, flows, and requirements traceability in the report.
- `model.yaml`: source for the component diagrams, class diagrams, sequence diagrams, and their descriptions in the report.
- `generated/`: D2 diagram sources and the PNG images referenced by the report. Both are generated from the above models and committed to Git to check for drift. Edit the models rather than the generated files. The delivery roadmap is drawn separately by the report generator.
- `report.docx`: current report and typesetting baseline; `report.template.docx`: course style reference. Both Word files are stored only locally and are not tracked by Git.

All diagram titles and labels are in English. The surrounding report prose remains in its original language. Historical implementation-status labels are retained as part of the report snapshot.

D2 diagrams use 32 px node labels and class members, 36 px container and class headings, and 44 px titles. Connection labels use bold, upright 48 px text in dark slate for readability, including `<<include>>`, `<<extend>>`, and sequence messages. Long labels wrap and ELK spacing is reduced to keep the text readable when graphics are fitted to the report's full 14 cm content width.

Diagram generation requires [D2](https://d2lang.com/tour/install/) **0.9.0** on `PATH`. The renderer checks this version and uses the bundled ELK layout engine, theme 0, and native PNG export to keep rendering consistent. Java and PlantUML are no longer required. D2 represents use cases as ovals inside a system boundary, preserves UML relationship types and multiplicities, and uses nested sequence groups for conditional branches, loops, and optional interactions. Multi-participant notes are attached to the first participant and identify their full scope in the label.

After modifying the structured model, run `uv run scripts/update_report.py` to synchronize the diagrams and local report. To regenerate diagrams without a local Word file, run:

```shell
uv run scripts/update_use_cases.py
uv run scripts/update_system_design.py
```

Run `uv run scripts/check.py --report` to check the use-case diagrams, design diagrams, and report generation logic. The diagram generators also accept `--check` to detect drift in both D2 sources and PNGs. The report generator only replaces the target areas for requirements analysis and system design; it does not rearrange the cover. Early detailed requirements drafts, ADRs, and reproducible view documents have been removed from the current directory; Git history can be consulted when traceability is needed.
