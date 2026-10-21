# Sample CoffeeScript code for testing
square = (x) -> x * x
console.log square(5)

class Animal
  constructor: (@name) ->
  move: (meters) -> alert @name + " moved #{meters}m."

if true
  x = 42
else
  x = 24

for item in [1, 2, 3]
  console.log item

# Write some coffeescript examples
# for this file to test syntax highlighting

# String interpolation and functions
greet = (name, age) ->
  "Hello #{name}, you are #{age} years old!"

console.log greet("Alice", 25)

# Array comprehensions
numbers = [1, 2, 3, 4, 5]
squares = (x * x for x in numbers)
evens = (num for num in numbers when num % 2 is 0)

# Object literals
person =
  name: "Bob"
  age: 30
  sayHello: -> "Hi, I'm #{@name}"

# Class inheritance
class Dog extends Animal
  constructor: (@name, @breed) ->
    super @name
  bark: -> console.log "#{@name} says woof!"

myDog = new Dog("Rex", "Golden Retriever")
myDog.bark()

# Switch statement
switch day = "Monday"
  when "Monday", "Tuesday"
    console.log "Beginning of week"
  when "Friday"
    console.log "TGIF!"
  else
    console.log "Regular day"

# Destructuring assignment
[first, second, ...rest] = [1, 2, 3, 4, 5]
{name, age} = person

# Range and splicing
range = [1..10]
countdown = [10..1]
firstThree = range[0..2]

templete = '''
  <div class="container">
    <div class="row">
      <div class="col-md-4">
        <h2>Heading</h2>
        <p>Donec id elit non mi porta gravida at eget metus. Fusce dapibus, tellus ac cursus commodo, tortor mauris condimentum nibh, ut fermentum massa justo sit amet risus. Etiam porta sem malesuada magna mollis euismod. Donec sed
        odio dui. </p>
      </div>
      </div>
    </div>
  </div>
'''

data =
  title: "CoffeeScript"
  body: "CoffeeScript is a little language that compiles into JavaScript."

template = (data) ->
  '''
  <div class="container">
    <div class="row">
      <div class="col-md-4">
        <h2>#{data.title}</h2>
        <p>#{data.body}</p>
      </div>
    </div>
  </div>
  '''


publish_template = template ->
  (data) ->
    html = template(data)
    $('body').append html

publish_template(template)(data)
