# Export FCS

##### Description

This operator can be used to write FCS files based on data projected onto the 
crosstab view.

##### Usage

Input data|.
---|---
`y-axis`        | Measurement value.
`row`           | Channels.
`column`        | Events.
`colors`        | Factor(s) used to split data into multiple FCS files.
`labels`        | Factor(s) used as annotations (e.g. UMAP coordinates, cluster ID) that will be added to expression data and converted as numeric values.

Output|.
---|---
`FCS Files`        | Files will be uploaded in the current project.
