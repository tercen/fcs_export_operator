# The files read back with flowCore (the reader most R users have): every value, every name,
# the marker descriptions and the population legend. Run where the example wrote its output:
#   docker run --rm -v "$PWD/out:/w" -w /w --entrypoint Rscript tercen/runtime-flowsuite:3.15-1 /w/flowcore_check.R
suppressMessages(library(flowCore))
unzip("sample.zip", exdir = "x")
fcs <- sort(list.files("x", pattern = "\\.fcs$"))
stopifnot(identical(fcs, c("sample 0.fcs", "sample 1.fcs", "sample 2.fcs")))
stopifnot(file.exists("x/levels.csv"))
exp <- read.csv("expected.csv", check.names = FALSE, stringsAsFactors = FALSE)
for (f in fcs) {
  ff <- read.FCS(file.path("x", f), transformation = FALSE, truncate_max_range = FALSE)
  m <- exprs(ff)
  want <- as.matrix(exp[exp$file == f, -1])
  stopifnot(identical(unname(colnames(m)), colnames(want)))
  # expected.csv holds each float32 in its shortest decimal form, which reads back as the nearest
  # double: equal to the file's float32 within half a float32 ulp (6e-8 relative).
  stopifnot(nrow(m) == nrow(want), max(abs(m - want) / pmax(1, abs(want))) < 1.2e-7)
  p <- pData(parameters(ff))
  stopifnot(identical(as.character(unname(p$desc[1:4])), c("CD3", "CD4", "CD8", "CD19")))
  stopifnot(identical(unname(as.character(keyword(ff)[["TERCEN_P7_LEVELS"]])), "1=B cells; 2=NK, CD16+; 3=T cells"))
  cat("ok", f, nrow(m), "events x", ncol(m), "\n")
}
