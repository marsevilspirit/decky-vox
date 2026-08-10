/** Narrow Steam input surface used by Decky Vox.
 *
 * @decky/ui declares the global SteamClient object; this local type documents
 * the two optional runtime capabilities for native text injection and Return.
 */
export interface DeckyVoxSteamInputCapabilities {
  ControllerKeyboardSendText?: (text: string) => void;
  ControllerKeyboardSetKeyState?: (key: number, pressed: boolean) => void;
}
