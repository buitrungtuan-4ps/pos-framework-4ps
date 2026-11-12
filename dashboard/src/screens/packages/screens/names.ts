// The catalogue's ids in the operator's words. A component, property, token, device class or act
// the console has no words for yet (a newer cloud's) shows its id, as a vendor's does (ADR-0153).

import { tFromServer } from "../../../i18n";

const dotted = (id: string) => id.replaceAll(".", "_");

/** A component's name. */
export const componentName = (id: string) => tFromServer(`designer.component.${id}`, id);

/** A property's name. */
export const propertyName = (name: string) => tFromServer(`designer.property.${name}`, name);

/** A token's name. */
export const tokenName = (token: string) => tFromServer(`designer.token.${token}`, token);

/** A device class's name. */
export const deviceName = (deviceClass: string) => tFromServer(`designer.device.${deviceClass}`, deviceClass);

/** An act's name. */
export const actionName = (actionId: string) => tFromServer(`designer.action.${dotted(actionId)}`, actionId);
