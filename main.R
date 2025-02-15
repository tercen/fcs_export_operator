suppressPackageStartupMessages({
  library(tercen)
  library(dplyr, warn.conflicts = FALSE)
  library(tidyr)
  library(flowCore)
})

source("./utils.R")

ctx = tercenCtx()

data <- ctx$as.matrix()

channels <- ctx$cselect() %>% 
  tidyr::unite(col = "new_name", sep = "_")

colnames(data) <- channels[["new_name"]]

annotation <- ctx$select(c(".ri", ctx$colors, ctx$labels)) %>% 
  distinct() %>%
  mutate_at(unlist(ctx$labels), ~as.numeric(as.factor(.)))

data <- data %>% 
  as_tibble() %>%
  mutate(.ri = 1:nrow(.) - 1L) %>%
  left_join(annotation, by = ".ri")

grouping_factor <- unlist(ctx$colors)
if(is.null(grouping_factor)) {
  grouping_factor <- "File"
  data <- data %>% mutate(File = "File")  
}

data <- data %>%
  group_by(across(contains(grouping_factor)))

flow.frames <- data %>% 
  group_map(~matrix_to_flowFrame(as.matrix(.x))) 

names(flow.frames) <- group_keys(data) %>% 
  tidyr::unite(col = "File", everything(), sep = " - ") %>%
  unlist()

flow.set <- flow.frames %>%
  flowCore::flowSet()

tmpdir <- paste0(tempdir(), "/fcs")
files <- write.flowSet(flow.set, outdir = tmpdir)
zipfile <- paste0(tmpdir, "/Tercen_FCS_Export.zip")
zip(zipfile = zipfile, files = files)

on.exit(unlink(c(zipfile, files)))

file_to_tercen(file_path = zipfile, filename = "Tercen_FCS_Export.zip") %>%
  ctx$addNamespace() %>%
  as_relation() %>%
  as_join_operator(list(), list()) %>%
  save_relation(ctx)
