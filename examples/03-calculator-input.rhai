// Advanced example: keep one resolved window and send keys to its HWND.
// Prerequisite: Calculator is open, frontmost, and in standard mode.
//
// The UIA click selects the operator. Digits and Enter use Region.press(), so
// this also exercises the rule that a resolved window remains the same target
// after activation changes the Z order.

fn press_each(window, keys) {
    for key in keys {
        window.press(key);
    }
}

// Windows Calculator is hosted by ApplicationFrameHost. UI Automation IDs
// below confirm that the frontmost hosted window is Calculator.
let calc = expect_window("exe=ApplicationFrameHost.exe").to_appear(5000);
expect(calc.target("ui:id=CalculatorResults,type=text")).to_appear(5000);
calc.target("ui:id=clearButton,type=button").click();

press_each(calc, ["1", "2"]);
calc.target("ui:id=plusButton,type=button").click();
press_each(calc, ["3", "4", "enter"]);

let result = calc.target("ui:id=CalculatorResults,type=text").resolve().region();
expect(result.target("ocr:46")).to_appear(5000);
print("12 + 34 = 46 verified through language-neutral HWND input");
