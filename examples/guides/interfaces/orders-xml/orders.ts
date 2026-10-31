import { quarantine, type XmlElement } from "@ductus/sdk";

/** A home-care equipment order for the supplier: the patient's MRN checked, the SSN taken out. */
export function transform(doc: XmlElement) {
  const mrn = doc.get("Patient/Identifiers/MRN"); // text at a path, "" if missing
  if (!mrn) throw quarantine("an order without Patient/Identifiers/MRN");
  doc.find("Patient")?.remove("SSN"); // remove child elements by name
  doc.set("Patient/Status", "verified"); // creates missing elements
  doc.set("@source", "ductus"); // an attribute, on the root
  const second = doc.get("Items/Item[2]/@sku"); // the second Item's attribute, 1-based
  if (second) doc.add("Note", `more than one item; the second is ${second}`);
  return doc;
}
