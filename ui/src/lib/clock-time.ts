// An instant as a clock time, hours and minutes, in the reader's own language: a printed check, a
// payment taken, a tab's last round.

import { locale } from "../i18n";

export function clockTime(time: string): string {
    return new Date(Date.parse(time)).toLocaleTimeString(
        locale() === "vi" ? "vi-VN" : "en-GB",
        {
            hour: "2-digit",
            minute: "2-digit",
        },
    );
}
