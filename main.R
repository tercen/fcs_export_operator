library(tercen)
library(dplyr, warn.conflicts = FALSE)
library(flowCore)

ctx = tercenCtx()

data <- ctx$as.matrix() %>% t()
channels <- ctx$rselect()[[1]]
colnames(data) <- channels

data <- data %>% 
  as_tibble() %>%
  mutate(.ci = 1:nrow(.) - 1L) %>%
  mutate(file = "File")

flow.frames <- data %>%
  group_by(across(contains(c("file")))) %>% 
  group_map(~tim::matrix_to_flowFrame(as.matrix(.x))) 

names(flow.frames) <- levels(as.factor(files[[1]]))

flow.set <- flow.frames %>%
  flowCore::flowSet()

tmpdir <- tempdir()
files <- write.flowSet(flow.set, outdir = paste0(tmpdir, "/fcs"))
zip(zipfile = 'Tercen_FCS_Export.zip', files = files)

on.exit(unlink('Tercen_FCS_Export.zip'))
on.exit(unlink(files))

tim::plot_file_to_df('Tercen_FCS_Export.zip', filename = "Tercen_FCS_Export.zip") %>%
  select(-checksum) %>%
  as_relation() %>%
  as_join_operator(list(), list()) %>%
  save_relation(ctx)