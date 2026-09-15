// Intermediate example: operate Windows Calculator through language-neutral
// UI Automation IDs. Prerequisite: Calculator is open in standard mode.
// Re-running is safe because the first action clears the current calculation.

target("ui:id=clearButton,type=button").click();
target("ui:id=num1Button,type=button").click();
target("ui:id=num2Button,type=button").click();
target("ui:id=plusButton,type=button").click();
target("ui:id=num3Button,type=button").click();
target("ui:id=num4Button,type=button").click();
target("ui:id=equalButton,type=button").click();

// The accessible result name is localized, so verify its numeric content with
// OCR inside the language-neutral result element.
let result = target("ui:id=CalculatorResults,type=text").resolve().region();
expect(result.target("ocr:46")).to_appear(5000);
print("12 + 34 = 46 verified with language-neutral UI Automation IDs");
