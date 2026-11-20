import * as vscode from "vscode";

const ductus = () => vscode.workspace.getConfiguration("ductus");

/** The environment tests, checks and debugging read the project in (`--env`). */
export const environment = () => ductus().get<string>("environment")?.trim() || "dev";

/** Whether saving a file of a project runs `ductus validate`. */
export const validateOnSave = () => ductus().get<boolean>("validateOnSave", true);
