# Paper

Source for the arXiv paper (SEA-357) is [main.tex](main.tex) with [refs.bib](refs.bib). The author line is Ryan Martin. Add an affiliation on the title page before submission.

The intended primary category is **cs.SE**. That choice is a submit-time label. It is not a claim in the paper.

Raw benchmark results are in [`measurements/`](measurements). The evaluation section quotes those files. Rerun them with `cargo run -p measure -- --repeats 10` after `TYPESAFE_API_KEY`, `OPENAI_API_KEY`, and `OPENAI_MODEL` are set.

Build the PDF from this directory:

```sh
tectonic main.tex
```

That command compiles [main.tex](main.tex) and [refs.bib](refs.bib). The PDF is a build product. [`.gitignore`](.gitignore) ignores `*.pdf`, so a local TeX PDF is not committed.
