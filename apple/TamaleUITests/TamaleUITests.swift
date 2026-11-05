import XCTest

/// Drives the app against a local tamale-hub and the stand-in session from src/web.rs. Run by
/// apple/scripts/uitest.sh, which passes TAMALE_DEBUG_KEY and TAMALE_DEBUG_HUB.
final class TamaleUITests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    /// Waits for `element`, printing the screen's elements if it doesn't come.
    func wait(_ element: XCUIElement, _ app: XCUIApplication, timeout: Double = 15, line: UInt = #line) {
        if element.waitForExistence(timeout: timeout) { return }
        print("SCREEN AT LINE \(line):\n\(app.debugDescription)")
        XCTFail("waited for \(element)", line: line)
    }

    func text(_ app: XCUIApplication, containing: String) -> XCUIElement {
        app.descendants(matching: .any).matching(NSPredicate(format: "label CONTAINS %@", containing)).firstMatch
    }

    /// The code /remote shows is a session link, without the key: scanned (here, pasted) on a
    /// fresh install, the app asks for the key, then opens the session. Starts
    /// from no keys at all.
    func testASessionCodeBeforeTheKeyOpensTheSessionOnceTheKeyIsAdded() throws {
        let env = ProcessInfo.processInfo.environment
        guard let key = env["TAMALE_DEBUG_KEY"], let hub = env["TAMALE_DEBUG_HUB"], let sid = env["TAMALE_DEBUG_SID"] else {
            throw XCTSkip("run apple/scripts/uitest.sh")
        }
        let app = XCUIApplication()
        app.launchEnvironment = ["TAMALE_DEBUG_RESET": "1"]
        app.launch()
        let add = app.buttons["Add a key"]
        wait(add, app, timeout: 10)
        add.tap()
        let field = app.textFields.firstMatch.exists ? app.textFields.firstMatch : app.textViews.firstMatch
        wait(field, app, timeout: 5)
        field.tap()
        field.typeText("\(hub)/open/\(sid)")
        wait(text(app, containing: "That's a session's code"), app, timeout: 5)
        let keyField = app.textFields.firstMatch.exists ? app.textFields.firstMatch : app.textViews.firstMatch
        keyField.tap()
        keyField.typeText(key)
        app.navigationBars["Add a key"].buttons["Add"].tap()
        // Straight into the session, once the sheet has gone.
        wait(text(app, containing: "Here it is."), app, timeout: 20)
    }

    /// Opens the stand-in session. Its approval stays up, for the tests that come after.
    func openSession(_ app: XCUIApplication) throws {
        let env = ProcessInfo.processInfo.environment
        guard let key = env["TAMALE_DEBUG_KEY"], let hub = env["TAMALE_DEBUG_HUB"] else {
            throw XCTSkip("run apple/scripts/uitest.sh")
        }
        app.launchEnvironment = ["TAMALE_DEBUG_KEY": key, "TAMALE_DEBUG_HUB": hub]
        app.launch()
        let row = text(app, containing: "Show me the plan")
        wait(row, app, timeout: 20)
        row.tap()
    }

    /// A long conversation opens at its end with only that loaded, and scrolling up brings in
    /// earlier pages without losing the place.
    func testALongSessionStartsAtTheEndAndPagesBack() throws {
        let app = XCUIApplication()
        try openSession(app)
        let end = text(app, containing: "Here it is.")
        wait(end, app, timeout: 20)
        XCTAssertTrue(end.isHittable, "it opens at the end")
        XCTAssertFalse(text(app, containing: "Old question 150").exists, "not loaded, or scrolled to")
        attach(app, "opened at the end")

        // Only the last 60 entries came, so this one's from a page that had to be fetched.
        let older = app.staticTexts.matching(NSPredicate(format: "label BEGINSWITH 'Old question 26'")).firstMatch
        let questions = app.staticTexts.matching(NSPredicate(format: "label BEGINSWITH 'Old question'"))
        func topQuestion() -> Int { Int(questions.firstMatch.label.split(separator: " ").last ?? "") ?? 0 }
        var swipes = 0
        var top = topQuestion()
        while !older.exists && swipes < 40 {
            app.scrollViews.firstMatch.swipeDown(velocity: .slow)
            swipes += 1
            // Steady steps, also as pages arrive: losing the place shows as a leap.
            let now = topQuestion()
            XCTAssertLessThan(top - now, 15, "swipe \(swipes) leapt from question \(top) to \(now)")
            top = now
        }
        wait(older, app, timeout: 5)
        XCTAssertGreaterThan(swipes, 2, "it took scrolling to get there")
        attach(app, "paged back")

        // Once the page has settled the view stays put: not thrown to the end or the top, and
        // not fetching page after page by itself.
        Thread.sleep(forTimeInterval: 1.5)
        let label = older.label
        let y = older.frame.minY
        Thread.sleep(forTimeInterval: 2)
        XCTAssertEqual(app.staticTexts[label].frame.minY, y, accuracy: 1)
        XCTAssertFalse(app.staticTexts["Old question 0"].exists, "it hasn't run to the top")
    }

    /// A reply that grows past the screen stays in view, and scrolling away stops it pulling.
    /// Named to run last: the tests share a session, and this buries the earlier entries and
    /// answers the approval the others look for.
    func testWhileAReplyStreamsTheViewFollowsUntilScrolledAway() throws {
        let app = XCUIApplication()
        try openSession(app)
        let field = app.textFields.firstMatch.exists ? app.textFields.firstMatch : app.textViews.firstMatch
        wait(field, app, timeout: 5)
        field.tap()
        field.typeText("stream")
        app.buttons["Send"].tap()
        let last = text(app, containing: "Streamed paragraph 59.")
        wait(last, app, timeout: 15)
        Thread.sleep(forTimeInterval: 0.5)
        XCTAssertLessThan(last.frame.maxY, app.scrollViews.firstMatch.frame.maxY + 1, "the end is on screen")

        // Scrolled up, a new entry doesn't take the view away.
        for _ in 0..<3 { app.scrollViews.firstMatch.swipeDown(velocity: .slow) }
        Thread.sleep(forTimeInterval: 1)
        let reading = text(app, containing: "Streamed paragraph 2")
        wait(reading, app, timeout: 5)
        let y = reading.frame.minY
        if app.buttons["Yes"].exists { app.buttons["Yes"].tap() }
        Thread.sleep(forTimeInterval: 1)
        XCTAssertTrue(reading.exists, "still reading the same part")
        XCTAssertLessThan(abs(reading.frame.minY - y), 300, "not thrown to the end")
    }

    /// The Changes sheet lists the working tree's files; a file's diff folds what's unchanged,
    /// and selected lines quote into the composer.
    func testShowsTheChangesAndAsksAboutALine() throws {
        let app = XCUIApplication()
        try openSession(app)
        let changes = app.buttons["Changes"]
        wait(changes, app, timeout: 20)
        wait(text(app, containing: "Here it is."), app, timeout: 20)
        changes.tap()

        let file = text(app, containing: "main.rs")
        wait(file, app, timeout: 15)
        XCTAssertTrue(text(app, containing: "gone.txt").exists)
        XCTAssertTrue(text(app, containing: "new.txt").exists)
        XCTAssertTrue(text(app, containing: "3 files").exists)
        attach(app, "file tree")
        file.tap()

        let changed = text(app, containing: "line thirty, changed")
        wait(changed, app, timeout: 5)
        XCTAssertTrue(text(app, containing: "26 unchanged lines").exists, "folded above the change")
        XCTAssertFalse(text(app, containing: "line 5").exists)
        attach(app, "file diff")

        text(app, containing: "26 unchanged lines").tap()
        wait(text(app, containing: "line 20"), app, timeout: 5)
        XCTAssertTrue(text(app, containing: "6 unchanged lines").exists, "opened next to the change")

        // The tree's order: directories first.
        app.buttons["Next File"].tap()
        wait(text(app, containing: "going"), app, timeout: 5)
        app.buttons["Next File"].tap()
        wait(text(app, containing: "brand new"), app, timeout: 5)
        app.buttons["Previous File"].tap()
        app.buttons["Previous File"].tap()

        wait(changed, app, timeout: 5)
        changed.tap()
        attach(app, "line selected")
        wait(app.buttons["Ask"], app, timeout: 5)
        app.buttons["Ask"].tap()

        let field = app.textFields.firstMatch.exists ? app.textFields.firstMatch : app.textViews.firstMatch
        let quoted = expectation(for: NSPredicate(format: "value CONTAINS 'src/main.rs'"), evaluatedWith: field)
        wait(for: [quoted], timeout: 10)
        XCTAssertTrue((field.value as? String)?.contains("+line thirty, changed") == true)
    }

    /// One tap asks for an artifact of what's being discussed.
    func testTheArtifactButtonSendsItsPrompt() throws {
        let app = XCUIApplication()
        try openSession(app)
        wait(text(app, containing: "Here it is."), app, timeout: 20)
        let button = app.buttons["Generate an artifact"]
        wait(button, app, timeout: 15)
        attach(app, "composer")
        button.tap()
        wait(text(app, containing: "Can you generate an artifact for this?"), app, timeout: 10)
        // It makes way while typing.
        let field = app.textFields.firstMatch.exists ? app.textFields.firstMatch : app.textViews.firstMatch
        field.tap()
        field.typeText("x")
        XCTAssertFalse(button.exists)
    }

    func attach(_ app: XCUIApplication, _ name: String) {
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = name
        shot.lifetime = .keepAlways
        add(shot)
    }

    /// The viewer gives the whole screen to the page, and its controls get out of the way.
    func testTheViewerFillsTheScreenAndItsControlsHideWhileReading() throws {
        let env = ProcessInfo.processInfo.environment
        guard let key = env["TAMALE_DEBUG_KEY"], let hub = env["TAMALE_DEBUG_HUB"] else {
            throw XCTSkip("run apple/scripts/uitest.sh")
        }
        let app = XCUIApplication()
        app.launchEnvironment = ["TAMALE_DEBUG_KEY": key, "TAMALE_DEBUG_HUB": hub]
        app.launch()
        let row = text(app, containing: "Show me the plan")
        wait(row, app, timeout: 20)
        row.tap()
        let yes = app.buttons["Yes"]
        wait(yes, app, timeout: 20)
        yes.tap()

        // Inline: the card runs edge to edge, and a short page gets a short card.
        let inline = app.webViews.firstMatch
        wait(inline, app, timeout: 15)
        wait(inline.staticTexts["Plan"], app, timeout: 15)
        XCTAssertEqual(inline.frame.width, app.frame.width, accuracy: 1)
        let short = NSPredicate { _, _ in inline.frame.height < 150 }
        wait(for: [expectation(for: short, evaluatedWith: nil)], timeout: 10)

        let field = app.textFields.firstMatch.exists ? app.textFields.firstMatch : app.textViews.firstMatch
        field.tap()
        field.typeText("long")
        app.buttons["Send"].tap()
        // An approval takes the keyboard down, leaving room to read what it's about.
        wait(yes, app, timeout: 15)
        XCTAssertFalse(app.keyboards.firstMatch.waitForExistence(timeout: 1))
        yes.tap()

        let card = app.buttons["Launch checklist"]
        wait(card, app, timeout: 15)
        let loaded = expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: card)
        wait(for: [loaded], timeout: 15)
        card.tap()

        let done = app.buttons["Done"]
        wait(done, app, timeout: 10)
        XCTAssertEqual(app.webViews.firstMatch.frame, app.frame)

        // Reading hides the controls, leaving a handle that brings them back.
        app.webViews.firstMatch.swipeUp()
        let handle = app.buttons["Show controls"]
        wait(handle, app, timeout: 5)
        XCTAssertFalse(done.exists)
        handle.tap()
        wait(done, app, timeout: 5)

        // Scrolling back up does too.
        app.webViews.firstMatch.swipeUp()
        wait(handle, app, timeout: 5)
        app.webViews.firstMatch.swipeDown()
        wait(done, app, timeout: 5)

        // The end of the page clears the bar.
        for _ in 0..<12 { app.webViews.firstMatch.swipeUp(velocity: .fast) }
        // (The conversation's card, underneath, has the same page.)
        let footer = app.webViews.firstMatch.staticTexts["The end of the page"]
        wait(footer, app, timeout: 5)
        wait(done, app, timeout: 5)
        XCTAssertLessThan(footer.frame.maxY, done.frame.minY)

        // The title goes back to the top, and from there a pull down closes it.
        // (The conversation's card, underneath, has the same name.)
        app.buttons["artifact-title"].tap()
        wait(app.webViews.firstMatch.staticTexts["Step 1"], app, timeout: 5)
        Thread.sleep(forTimeInterval: 1)
        let web = app.webViews.firstMatch
        web.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.3))
            .press(forDuration: 0.05, thenDragTo: web.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.95)))
        let closed = expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: done)
        wait(for: [closed], timeout: 5)
        wait(card, app, timeout: 5)
    }

    func testAddsAKeyOpensASessionAndDrivesIt() throws {
        let env = ProcessInfo.processInfo.environment
        guard let key = env["TAMALE_DEBUG_KEY"], let hub = env["TAMALE_DEBUG_HUB"] else {
            throw XCTSkip("run apple/scripts/uitest.sh")
        }
        let app = XCUIApplication()
        app.launchEnvironment = ["TAMALE_DEBUG_KEY": key, "TAMALE_DEBUG_HUB": hub]
        app.launch()

        // The list: the stand-in session, its sealed title opened with the key.
        let row = text(app, containing: "Show me the plan")
        wait(row, app, timeout: 20)
        XCTAssertTrue(text(app, containing: "fake-host:~/dev/fake").exists)
        row.tap()

        // The conversation, over the encrypted channel.
        wait(text(app, containing: "Here it is."), app, timeout: 20)
        wait(text(app, containing: "The plan"), app, timeout: 10)

        // The approval.
        let yes = app.buttons["Yes"]
        wait(yes, app, timeout: 10)
        XCTAssertTrue(text(app, containing: "Allow bash?").exists)
        yes.tap()
        wait(text(app, containing: "approved: Yes"), app, timeout: 10)

        // A prompt, and its reply with an artifact.
        let field = app.textFields.firstMatch.exists ? app.textFields.firstMatch : app.textViews.firstMatch
        wait(field, app, timeout: 5)
        field.tap()
        field.typeText("hello from the phone")
        app.buttons["Send"].tap()
        wait(text(app, containing: "You said hello from the phone"), app, timeout: 15)

        // The artifact, full screen, rendered by WebKit.
        let echo = app.buttons.matching(NSPredicate(format: "label CONTAINS %@", "Echo")).firstMatch
        wait(echo, app, timeout: 15)
        let inline = app.webViews.staticTexts["hello from the phone"]
        wait(inline, app, timeout: 15)
        // Enabled once the page has arrived.
        let loaded = expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: echo)
        wait(for: [loaded], timeout: 15)
        echo.tap()
        wait(app.buttons["Done"], app, timeout: 10)
        XCTAssertEqual(app.buttons["artifact-title"].label, "Echo")
        app.buttons["Done"].tap()

        app.navigationBars.buttons.element(boundBy: 0).tap()
        // The library keeps what was opened.
        app.buttons["Library"].tap()
        wait(text(app, containing: "Echo"), app, timeout: 10)
        XCTAssertTrue(text(app, containing: "The plan").exists)

        // After a relaunch, without the test key: the key came back from the Keychain and
        // the library from the store.
        app.terminate()
        app.launchEnvironment = [:]
        app.launch()
        wait(text(app, containing: "Show me the plan"), app, timeout: 20)
        app.buttons["Library"].tap()
        wait(text(app, containing: "Echo"), app, timeout: 10)
        text(app, containing: "Echo").tap()
        wait(app.webViews.staticTexts["hello from the phone"], app, timeout: 10)
        app.buttons["Done"].tap()
    }

    /// The agent's questions: options to tap, one or several, an answer of your own, and the
    /// answers sent together.
    func testAnswersTheAgentsQuestions() throws {
        let env = ProcessInfo.processInfo.environment
        guard let key = env["TAMALE_DEBUG_KEY"], let hub = env["TAMALE_DEBUG_HUB"] else {
            throw XCTSkip("run apple/scripts/uitest.sh")
        }
        let app = XCUIApplication()
        app.launchEnvironment = ["TAMALE_DEBUG_KEY": key, "TAMALE_DEBUG_HUB": hub]
        app.launch()
        let row = text(app, containing: "Show me the plan")
        wait(row, app, timeout: 20)
        row.tap()
        wait(text(app, containing: "Here it is."), app, timeout: 20)
        // Whatever approval an earlier test left.
        let yes = app.buttons["Yes"]
        if yes.waitForExistence(timeout: 3) { yes.tap() }

        let field = app.textFields.firstMatch.exists ? app.textFields.firstMatch : app.textViews.firstMatch
        wait(field, app, timeout: 5)
        field.tap()
        field.typeText("ask")
        app.buttons["Send"].tap()

        wait(text(app, containing: "Which channel should it go to?"), app, timeout: 15)
        XCTAssertTrue(text(app, containing: "Testers first").exists)
        XCTAssertFalse(app.keyboards.firstMatch.waitForExistence(timeout: 1))
        let next = app.buttons["Next"]
        XCTAssertFalse(next.isEnabled, "nothing picked yet")
        app.buttons["Stable"].tap()
        next.tap()

        wait(text(app, containing: "What should the notes cover?"), app, timeout: 5)
        app.buttons["Fixes"].tap()
        app.buttons["Features"].tap()
        let own = app.textFields["Your own answer"].exists ? app.textFields["Your own answer"] : app.textViews["Your own answer"]
        own.tap()
        own.typeText("docs")
        app.buttons["Send answers"].tap()
        wait(text(app, containing: "answered: Stable | Fixes + Features + docs"), app, timeout: 15)
        XCTAssertFalse(app.buttons["Send answers"].exists)
    }
}
