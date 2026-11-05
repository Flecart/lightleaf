# PaperMentor review pack: third-party material

The prompts (`prompts/`), writing guides (`skills/`) and comment texts (`fixed/`) in this directory are copied
without changes from the **AI Tutor / PaperMentor** review system by Jiarui Liu and contributors, in the
Overleaf fork at <https://github.com/jiarui-liu/overleaf> (commit `879ff960fee4482200957e04541aaaf2761a092f`,
path `services/web/app/src/Features/Chat/`). That repository is licensed under the
**GNU Affero General Public License v3.0**, and so is this material.

Upstream citation (from the original README):

> Liu, J. et al. *PaperMentor* (2026). https://github.com/jiarui-liu/overleaf

The Lightleaf code that runs the pack (`reviewer/src/review/packs/papermentor/`) reimplements the upstream
pipeline in Rust and is part of Lightleaf (AGPL-3.0-only).

## Not included

- `prototype_papers/`, `example_papers/`, `review_sets/`: full texts, PDFs and reviews of published papers
  by third parties. The upstream "Sentence Placement" reviewer needs them, so it is not part of this pack.
- The upstream "Acknowledgments Editor" and "Affiliations Editor": they edit papers according to one lab's
  house rules (funding text, citations of PaperMentor itself, specific institutes) rather than review them.
- Role-model paper uploads (not yet supported by Lightleaf's UI).

Some guides quote short passages of published papers as examples (for instance `skills/04_paper_sections/abstract.md`
and `introduction.md`); they are reproduced as they appear upstream.

## Licensing note for Lightleaf distributors

Because this material is third-party AGPL text, Lightleaf's maintainer cannot relicense it. A build offered
under other terms must leave out this directory and use the `lightleaf` pack (`LIGHTLEAF_REVIEW_PACK=lightleaf`),
whose prompts are Lightleaf's own.
