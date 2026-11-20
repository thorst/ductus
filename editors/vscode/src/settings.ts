import * as vscode from "vscode";

const ductus = () => vscode.workspace.getConfiguration("ductus");

/** The environment tests, checks and debugging read the project in (`--env`). */
export const environment = () => ductus().get<string>("environment")?.trim() || "dev";

/** Whether saving a file of a project runs `ductus validate`. */
export const validateOnSave = () => ductus().get<boolean>("validateOnSave", true);

/** Whether saving a file of a project deploys it to its running `ductus dev`. */
export const deployToDevOnSave = () => ductus().get<boolean>("deployToDevOnSave", false);

/** The engines Promote… deploys to and fetches from, by name (`{ "test": "https://…" }`). */
export const engines = (): unknown => ductus().get<unknown>("engines");
